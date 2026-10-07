//! Filling an index whose rows are missing.
//!
//! What makes this live above `doclite-core` rather than in an engine: an
//! object is bytes down there, and turning one into index rows means decoding
//! it and walking [`Record::rows`](crate::Record::rows). ADR 0001 said an index
//! can be rebuilt from the objects it names; this is where that is cashed in,
//! and the layer that knows what an object means is the only one that could.
//!
//! # Why it pages `<record>_latest`
//!
//! There is no way to enumerate objects. `Engine::read` takes scans and object
//! ids, and the ids are point lookups, so "every object of this type" is not a
//! question the read seam can be asked. The generated index is the answer: it
//! holds exactly one row per live record, so paging it is paging the records,
//! and every row it yields names a revision that is current.
//!
//! # Why a page is one transaction and a document is one assertion
//!
//! One operation per document would be one commit per document, and a durable
//! engine fsyncs a commit, so a store of any size would take hours. A page is
//! therefore one operation.
//!
//! The assertion cannot be batched away with it. A writer that supersedes a
//! revision between the scan and the commit has already retired its rows and
//! filed the new ones, so filing the scanned revision's rows afterwards leaves
//! a row naming a revision that is no longer live, and a later read would hand
//! that dead revision to a caller as if it were current. So every document in
//! the page carries `require_current` over its own run of abort codes, the way
//! a `Transaction` numbers the writes it stages. A page that aborts names the
//! one document that lost its race, drops it, and files the rest.

use doclite_core::{key, Bound, Engine, IndexId, Operation, Outcome};

use crate::connection::Shared;
use crate::error::DocumentError;
use crate::gate::Building;
use crate::identity::{DocumentId, RevisionId};
use crate::query::scan_between;
use crate::record::{Codes, CODE_BASE};
use crate::records::{Filled, Rebuilds, Records, Revised};
use crate::write::Refusals;

/// How many live records one transaction files the rows of.
///
/// A page is a commit and a commit on a durable engine is an fsync, so this
/// trades a bounded amount of memory for the thing that actually costs: a
/// million records is four thousand commits rather than a million.
const PAGE: usize = 256;

/// Fills every index the engine said has rows missing.
///
/// The gate opens per index as that index finishes rather than once at the end,
/// so a reader of one record type is not held up by a large one still being
/// walked.
pub(crate) async fn drive<E: Engine>(
    shared: &Shared<E>,
    records: &Records,
    mut building: Building,
) -> Result<(), DocumentError> {
    match fill(shared, records, &mut building).await {
        Ok(()) => Ok(()),
        Err(error) => {
            building.failed(&error);
            Err(error)
        }
    }
}

/// Fills every outstanding index, or stops at the first refusal.
///
/// Split from [`drive`] so recording the refusal is not something a caller has
/// to remember: a rebuild handed to a spawner reports through the gate and
/// nowhere else, because nothing is waiting on its return value.
async fn fill<E: Engine>(
    shared: &Shared<E>,
    records: &Records,
    building: &mut Building,
) -> Result<(), DocumentError> {
    for held in records.types() {
        let wanted = outstanding(held.as_ref(), shared.db.building());
        if wanted.is_empty() {
            continue;
        }

        match wanted.contains(&held.latest().id()) {
            true => unheld_type(held.as_ref(), &wanted)?,
            false => walk(shared, held.as_ref(), &wanted).await?,
        }

        for index in wanted {
            shared.db.rebuilt(index).await?;
            building.filled(index)?;
        }
    }
    Ok(())
}

/// Which of this type's indexes have rows missing.
fn outstanding(held: &dyn Rebuilds, building: &[IndexId]) -> Vec<IndexId> {
    held.schemas()
        .into_iter()
        .map(|schema| schema.id)
        .filter(|index| building.contains(index))
        .collect()
}

/// What it means when the index the others are derived from is itself missing
/// rows.
///
/// Two states produce it and only one is explainable. A record type the store
/// has never held has *every* index missing rows, the generated one included,
/// and there is nothing to walk: no revision of it exists, so an empty index is
/// already every row it owes. That is adding a struct to an existing database,
/// which has to keep working and is the ordinary case here.
///
/// The other has lost its enumeration source while indexes derived from it
/// survived. Nothing in this crate produces that and nothing in it can undo it:
/// the rows that would say which revisions are live are the ones that are gone.
/// It is refused rather than guessed at, which is what ADR 0010 does with an
/// envelope it cannot read.
fn unheld_type(held: &dyn Rebuilds, wanted: &[IndexId]) -> Result<(), DocumentError> {
    match wanted.len() == held.schemas().len() {
        true => Ok(()),
        false => Err(DocumentError::MissingLatest {
            index: held.latest().id(),
        }),
    }
}

/// Pages every live revision of one type and files the rows it owes.
async fn walk<E: Engine>(
    shared: &Shared<E>,
    held: &dyn Rebuilds,
    wanted: &[IndexId],
) -> Result<(), DocumentError> {
    let latest = held.latest();
    let mut after: Option<DocumentId> = None;

    loop {
        let from = after.map(|document| (key![document], Bound::Excl));
        let page = scan_between::<2, 1>(&latest, from, None)
            .limit(PAGE)
            .with_objects_lenient::<Vec<u8>>();

        let rows = match shared.db.run(&page).await? {
            Outcome::Committed(rows) => rows,
            Outcome::Aborted(code) => return Err(DocumentError::Aborted(code)),
        };
        if rows.is_empty() {
            return Ok(());
        }

        let revisions: Vec<(RevisionId, Vec<u8>)> = rows
            .into_iter()
            .filter_map(|(row, object)| object.map(|bytes| (RevisionId(row.object), bytes)))
            .collect();

        match file(shared, held, wanted, &revisions).await? {
            Some(document) => after = Some(document),
            None => return Ok(()),
        }
    }
}

/// Files one page as one transaction, and answers with its last document so the
/// next page starts after it.
///
/// A refusal that says the revision was superseded drops that document and
/// files the rest. Dropping it loses nothing: whoever superseded it filed the
/// new rows itself, in every declared index, so the row this was going to file
/// is already there under the revision that replaced it.
///
/// A refusal that says a key is taken is a different thing and is reported. An
/// index carries no constraint (ADR 0001), so a `#[unique]` index declared over
/// data that already holds two of a key cannot be filled, and saying so names
/// the index rather than leaving two rows nothing can tell apart.
async fn file<E: Engine>(
    shared: &Shared<E>,
    held: &dyn Rebuilds,
    wanted: &[IndexId],
    revisions: &[(RevisionId, Vec<u8>)],
) -> Result<Option<DocumentId>, DocumentError> {
    let mut superseded: Vec<RevisionId> = Vec::new();
    let mut last: Option<DocumentId> = None;

    loop {
        let mut operation = Operation::new();
        let mut staged: Vec<(RevisionId, Refusals)> = Vec::new();
        let mut next = CODE_BASE;

        for (revision, object) in revisions {
            if superseded.contains(revision) {
                continue;
            }
            let Filled { write, document } = held.fill(
                Revised {
                    object,
                    revision: *revision,
                    wanted,
                },
                Codes::at(next),
            )?;
            let (steps, refusals) = write.split();
            next += refusals.span();
            staged.push((*revision, refusals));
            operation = operation.then(steps);
            last = Some(document);
        }

        if staged.is_empty() {
            return Ok(last);
        }

        match shared.db.run(&operation).await? {
            Outcome::Committed(()) => return Ok(last),
            Outcome::Aborted(code) => {
                let Some((revision, refusals)) =
                    staged.iter().find(|(_, refusals)| refusals.owns(code))
                else {
                    return Err(DocumentError::Aborted(code));
                };
                match refusals.refusal(code) {
                    DocumentError::Stale => superseded.push(*revision),
                    taken => return Err(taken),
                }
            }
        }
    }
}

/// Refuses to open when indexes need filling and nothing here can fill them.
///
/// `Connection::over` takes schemas, which say which indexes exist and nothing
/// about how a row for one is derived. Naming the first index that needs a
/// record type is the honest answer: parking every read of it forever would be
/// worse, and answering out of it would be wrong.
pub(crate) fn unfillable(building: &[IndexId]) -> Result<(), DocumentError> {
    match building.first() {
        None => Ok(()),
        Some(index) => Err(DocumentError::NoRebuilder { index: *index }),
    }
}

#[cfg(test)]
mod tests {
    use doclite_core::{ColumnType, Index, IndexSchema};

    use super::*;

    static LATEST: Index<2> = Index::typed("thing_latest", &[ColumnType::U64, ColumnType::U64]);
    static BY_NAME: Index<2> = Index::typed("thing_by_name", &[ColumnType::Str, ColumnType::U64]);

    struct Thing;

    impl Rebuilds for Thing {
        fn schemas(&self) -> Vec<IndexSchema> {
            vec![BY_NAME.schema(), LATEST.schema()]
        }

        fn latest(&self) -> Index<2> {
            LATEST
        }

        fn fill(&self, _revised: Revised<'_>, _codes: Codes) -> Result<Filled, DocumentError> {
            Err(DocumentError::NotFound)
        }
    }

    /// Adding a struct to a database that already holds other records. Every
    /// index of it is missing rows because the type is new, there is nothing to
    /// walk, and this has to be allowed or the most ordinary migration there is
    /// becomes impossible.
    #[test]
    fn a_record_type_the_store_has_never_held_is_not_a_refusal() {
        let wanted = vec![BY_NAME.id(), LATEST.id()];
        assert!(unheld_type(&Thing, &wanted).is_ok());
    }

    /// The unexplainable one: the enumeration source is gone and an index
    /// derived from it survived, so the rows naming the live revisions are the
    /// ones that are missing.
    #[test]
    fn a_lost_generated_index_beside_a_filled_one_is_refused() {
        let wanted = vec![LATEST.id()];
        assert_eq!(
            unheld_type(&Thing, &wanted),
            Err(DocumentError::MissingLatest { index: LATEST.id() })
        );
    }

    #[test]
    fn only_this_types_indexes_are_claimed_from_what_is_building() {
        let building = vec![BY_NAME.id(), IndexId("other_by_thing")];
        assert_eq!(outstanding(&Thing, &building), vec![BY_NAME.id()]);
    }

    #[test]
    fn a_handle_with_no_record_types_refuses_rather_than_parking() {
        assert_eq!(
            unfillable(&[BY_NAME.id()]),
            Err(DocumentError::NoRebuilder {
                index: BY_NAME.id()
            })
        );
        assert!(unfillable(&[]).is_ok());
    }
}
