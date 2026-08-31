//! What filing a record and superseding one are, as operations.
//!
//! Every rule about the order of an update's steps lives in [`supersede`] and
//! nowhere else. That matters more than it reads: the order is not a
//! preference. `require_current` has to be asserted before the old rows are
//! retired, or read-your-own-writes shows it the delete of the very row it is
//! counting and every update in the store is stale. `require_unique` has to
//! come after them, or a record editing an unrelated field finds its own key
//! still in place and reports itself as a duplicate. Between the two there is
//! nothing left to decide.
//!
//! An operation is a value (ADR 0003), so both of these are ordinary functions
//! returning one. Nothing here needs an engine, which is what lets the tests at
//! the bottom of this file read the steps rather than run them.

use doclite_core::{exact, key, AbortCode, Index, Key, Operation};

use crate::error::DocumentError;
use crate::identity::{DocumentId, RevisionId};
use crate::record::{Codes, Reason, Record, Rows};
use crate::revision::Revision;

/// An operation and the names that turn its abort codes back into a reason.
pub(crate) struct Write {
    operation: Operation,
    refusals: Refusals,
}

/// What a write's abort codes stood for, kept apart from the operation so a
/// composed transaction can take the steps and leave the naming behind.
///
/// The names are collected by the same walk that numbered them, so the code an
/// abort comes back with and the name it resolves to cannot drift apart.
pub(crate) struct Refusals {
    codes: Codes,
    claimed: Vec<&'static str>,
}

impl Write {
    pub(crate) fn operation(&self) -> &Operation {
        &self.operation
    }

    pub(crate) fn refusal(&self, code: AbortCode) -> DocumentError {
        self.refusals.refusal(code)
    }

    /// Hands the steps to whoever is composing them and keeps the naming.
    pub(crate) fn split(self) -> (Operation, Refusals) {
        (self.operation, self.refusals)
    }
}

impl Refusals {
    /// Where the next write in the same operation may start numbering.
    pub(crate) fn span(&self) -> u32 {
        Codes::span(self.claimed.len())
    }

    /// Whether this write is the one an abort code came from.
    pub(crate) fn owns(&self, code: AbortCode) -> bool {
        self.codes.reason(code, self.claimed.len()).is_some()
    }

    /// Names why the operation refused, and for a taken unique key which index
    /// refused it, so the caller learns which field was taken rather than that
    /// something was.
    pub(crate) fn refusal(&self, code: AbortCode) -> DocumentError {
        match self.codes.reason(code, self.claimed.len()) {
            Some(Reason::Stale) => DocumentError::Stale,
            Some(Reason::Duplicate(at)) => match self.claimed.get(at) {
                Some(index) => DocumentError::Duplicate { index },
                None => DocumentError::Aborted(code),
            },
            None => DocumentError::Aborted(code),
        }
    }
}

/// Files a new record: an absence check per unique index, the revision itself,
/// and a row per index.
pub(crate) fn insert<T: Record>(
    envelope: &Revision<T>,
    document: DocumentId,
    revision: RevisionId,
    codes: Codes,
) -> Write {
    let value: &T = envelope;
    let claim = Claim::walk(value, document, revision, Operation::new(), codes);
    let operation = File::walk(
        value,
        document,
        revision,
        claim.operation.put_object(revision.0, envelope),
    );

    Write {
        operation,
        refusals: Refusals {
            codes,
            claimed: claim.claimed,
        },
    }
}

/// Supersedes a record: the old rows retired, the new ones filed, and the old
/// revision asserted to still be the live one.
///
/// `was` is the value as it was read, because the rows to retire are keyed on
/// what it held rather than on what it now holds.
pub(crate) fn supersede<T: Record>(
    envelope: &Revision<T>,
    was: &T,
    document: DocumentId,
    old: RevisionId,
    new: RevisionId,
    codes: Codes,
) -> Write {
    let now: &T = envelope;
    let retired = Retire::walk(
        was,
        document,
        old,
        require_current::<T>(Operation::new(), document, old, codes),
    );
    let claim = Claim::walk(now, document, new, retired, codes);
    let operation = File::walk(
        now,
        document,
        new,
        claim.operation.put_object(new.0, envelope),
    );

    Write {
        operation,
        refusals: Refusals {
            codes,
            claimed: claim.claimed,
        },
    }
}

/// Refuses the operation unless this revision is still the record's live one,
/// which is what makes a concurrent supersede a conflict rather than a lost
/// write.
///
/// It asks the generated index and nothing else, so it needs no value: the
/// question is which revision is current, not what it holds.
fn require_current<T: Record>(
    op: Operation,
    document: DocumentId,
    revision: RevisionId,
    codes: Codes,
) -> Operation {
    op.require_count(
        &T::latest(),
        exact(key![document, revision]),
        1,
        codes.stale(),
    )
}

/// Retires the row this value holds in every index.
struct Retire(Operation);

impl Retire {
    fn walk<T: Record>(
        value: &T,
        document: DocumentId,
        revision: RevisionId,
        op: Operation,
    ) -> Operation {
        let mut into = Self(op);
        value.rows(document, revision, &mut into);
        into.0
    }
}

impl Rows for Retire {
    fn row<const N: usize>(
        &mut self,
        index: &Index<N>,
        key: Key<N>,
        _name: &'static str,
        _unique: bool,
    ) {
        let op = std::mem::replace(&mut self.0, Operation::new());
        self.0 = op.delete_index(index, exact(key));
    }
}

/// Files a row in every index, all of them naming this revision.
struct File(Operation, RevisionId);

impl File {
    fn walk<T: Record>(
        value: &T,
        document: DocumentId,
        revision: RevisionId,
        op: Operation,
    ) -> Operation {
        let mut into = Self(op, revision);
        value.rows(document, revision, &mut into);
        into.0
    }
}

impl Rows for File {
    fn row<const N: usize>(
        &mut self,
        index: &Index<N>,
        key: Key<N>,
        _name: &'static str,
        _unique: bool,
    ) {
        let op = std::mem::replace(&mut self.0, Operation::new());
        self.0 = op.put_index(index, key, self.1);
    }
}

/// Refuses the operation if any unique key this value claims is taken, and
/// remembers which index was given which code on the way past.
struct Claim {
    operation: Operation,
    codes: Codes,
    claimed: Vec<&'static str>,
}

impl Claim {
    fn walk<T: Record>(
        value: &T,
        document: DocumentId,
        revision: RevisionId,
        op: Operation,
        codes: Codes,
    ) -> Self {
        let mut into = Self {
            operation: op,
            codes,
            claimed: Vec::new(),
        };
        value.rows(document, revision, &mut into);
        into
    }
}

impl Rows for Claim {
    fn row<const N: usize>(
        &mut self,
        index: &Index<N>,
        key: Key<N>,
        name: &'static str,
        unique: bool,
    ) {
        if !unique {
            return;
        }
        let at = self.claimed.len();
        self.claimed.push(name);
        let code = self.codes.duplicate(at);
        let op = std::mem::replace(&mut self.operation, Operation::new());
        self.operation = op.require_absent(index, exact(key), code);
    }
}

#[cfg(test)]
mod tests {
    use doclite_core::{Column, ColumnType, IndexId, ObjectId, RangeSpec, Step, ToOrdBytes};
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::clock::Timestamp;
    use crate::record::CODE_BASE;

    /// A record with one unique index and one plain one, so a walk yields both
    /// kinds and the order of what comes back is readable.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Note {
        slug: String,
        shelf: u8,
    }

    const LATEST: Index<2> = Index::typed("note_latest", &[ColumnType::U64, ColumnType::U64]);
    const BY_SLUG: Index<1> = Index::typed("note_by_slug", &[ColumnType::Str]);
    const BY_SHELF: Index<2> = Index::typed("note_by_shelf", &[ColumnType::U8, ColumnType::U64]);

    doclite_core::serde_storage!(Note);

    /// Written by hand rather than derived, because this crate cannot depend on
    /// the macro that expands into it. It is the shape the derive emits.
    impl Record for Note {
        fn schemas() -> Vec<doclite_core::IndexSchema> {
            vec![LATEST.schema(), BY_SLUG.schema(), BY_SHELF.schema()]
        }

        fn latest() -> Index<2> {
            LATEST
        }

        fn rows<R: Rows>(&self, document: DocumentId, revision: RevisionId, into: &mut R) {
            into.row(&LATEST, key![document, revision], "note_latest", false);
            into.row(&BY_SLUG, key![self.slug.clone()], "note_by_slug", true);
            into.row(
                &BY_SHELF,
                key![self.shelf, document],
                "note_by_shelf",
                false,
            );
        }
    }

    fn note(slug: &str, shelf: u8) -> Note {
        Note {
            slug: slug.into(),
            shelf,
        }
    }

    fn document(id: u64) -> DocumentId {
        DocumentId(ObjectId(id))
    }

    fn revision(id: u64) -> RevisionId {
        RevisionId(ObjectId(id))
    }

    fn envelope(value: Note, id: u64, prev: Option<u64>) -> Revision<Note> {
        Revision::new(value, document(id), Timestamp(1), prev.map(revision))
    }

    fn slot() -> Codes {
        Codes::at(CODE_BASE)
    }

    fn names(steps: &[Step]) -> Vec<&'static str> {
        steps
            .iter()
            .map(|step| match step {
                Step::Require { .. } => "require",
                Step::RequireObject { .. } => "require object",
                Step::PutObject { .. } => "put object",
                Step::PutIndex { .. } => "put index",
                Step::DeleteIndex { .. } => "delete index",
                Step::ForEach { .. } => "for each",
            })
            .collect()
    }

    #[test]
    fn filing_a_record_claims_its_unique_keys_before_it_writes_anything() {
        let write = insert(
            &envelope(note("n-1", 1), 7, None),
            document(7),
            revision(7),
            slot(),
        );

        assert_eq!(
            names(write.operation().steps()),
            [
                "require",
                "put object",
                "put index",
                "put index",
                "put index"
            ],
            "one claim, the revision, and a row per index"
        );
    }

    #[test]
    fn filing_a_record_files_a_row_in_every_index_including_the_generated_one() {
        let write = insert(
            &envelope(note("n-1", 1), 7, None),
            document(7),
            revision(7),
            slot(),
        );

        let filed: Vec<&str> = write
            .operation()
            .steps()
            .iter()
            .filter_map(|step| match step {
                Step::PutIndex { index, .. } => Some(index.0),
                _ => None,
            })
            .collect();

        assert_eq!(filed, ["note_latest", "note_by_slug", "note_by_shelf"]);
    }

    /// The order is not a preference. `require_current` before the deletes, or
    /// read-your-own-writes shows it the delete of the row it is counting;
    /// `require_absent` after them, or the record finds its own key.
    #[test]
    fn superseding_asserts_then_retires_then_claims_then_writes() {
        let write = supersede(
            &envelope(note("n-1", 2), 7, Some(7)),
            &note("n-1", 1),
            document(7),
            revision(7),
            revision(9),
            slot(),
        );

        assert_eq!(
            names(write.operation().steps()),
            [
                "require",
                "delete index",
                "delete index",
                "delete index",
                "require",
                "put object",
                "put index",
                "put index",
                "put index"
            ]
        );
    }

    #[test]
    fn the_first_step_of_a_supersede_counts_the_revision_being_replaced() {
        let write = supersede(
            &envelope(note("n-1", 2), 7, Some(7)),
            &note("n-1", 1),
            document(7),
            revision(7),
            revision(9),
            slot(),
        );

        let Some(Step::Require {
            index, range, code, ..
        }) = write.operation().steps().first()
        else {
            panic!("a supersede opens by asserting the revision is still the head");
        };

        assert_eq!(*index, IndexId("note_latest"));
        assert_eq!(*code, slot().stale());
        assert_eq!(
            *range,
            RangeSpec::Exact(vec![
                Column::encode(&document(7)),
                Column::encode(&revision(7)),
            ])
        );
    }

    /// The rows retired are keyed on what the value was, not on what it now is,
    /// which is the whole reason `supersede` is handed both.
    #[test]
    fn superseding_retires_the_rows_the_old_value_held() {
        let write = supersede(
            &envelope(note("moved", 2), 7, Some(7)),
            &note("original", 1),
            document(7),
            revision(7),
            revision(9),
            slot(),
        );

        let retired: Vec<RangeSpec> = write
            .operation()
            .steps()
            .iter()
            .filter_map(|step| match step {
                Step::DeleteIndex { index, range } if index.0 == "note_by_slug" => {
                    Some(range.clone())
                }
                _ => None,
            })
            .collect();

        assert_eq!(
            retired,
            [RangeSpec::Exact(vec![Column::Literal(
                "original".to_ord_bytes()
            )])],
            "the old slug, not the new one"
        );
    }

    #[test]
    fn a_claimed_index_resolves_the_code_it_was_given() {
        let write = insert(
            &envelope(note("n-1", 1), 7, None),
            document(7),
            revision(7),
            slot(),
        );

        assert_eq!(
            write.refusal(slot().duplicate(0)),
            DocumentError::Duplicate {
                index: "note_by_slug"
            }
        );
    }

    /// A write's first code is its stale one, so it must not be read as the
    /// first unique index the walk claimed.
    #[test]
    fn the_stale_code_is_not_read_as_a_duplicate() {
        let write = insert(
            &envelope(note("n-1", 1), 7, None),
            document(7),
            revision(7),
            slot(),
        );

        assert_eq!(write.refusal(slot().stale()), DocumentError::Stale);
    }

    #[test]
    fn a_code_no_index_claimed_is_left_as_it_came() {
        let write = insert(
            &envelope(note("n-1", 1), 7, None),
            document(7),
            revision(7),
            slot(),
        );
        let stray = slot().duplicate(7);

        assert_eq!(write.refusal(stray), DocumentError::Aborted(stray));
        assert!(!write.split().1.owns(stray));
    }

    /// The reason a code carries a base at all: two writes numbered from the
    /// same place would both answer to it, and an abort could not say which
    /// record refused.
    #[test]
    fn a_write_owns_only_the_codes_it_was_numbered_from() {
        let first = insert(
            &envelope(note("n-1", 1), 7, None),
            document(7),
            revision(7),
            slot(),
        )
        .split()
        .1;
        let next = Codes::at(CODE_BASE + first.span());
        let second = insert(
            &envelope(note("n-2", 1), 8, None),
            document(8),
            revision(8),
            next,
        )
        .split()
        .1;

        assert!(first.owns(slot().duplicate(0)));
        assert!(!second.owns(slot().duplicate(0)));
        assert!(second.owns(next.duplicate(0)));
        assert!(!first.owns(next.duplicate(0)));
    }
}
