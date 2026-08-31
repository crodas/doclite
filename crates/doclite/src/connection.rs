//! A handle that files records, finds them, and supersedes them.

use doclite_core::{key, Bound, Db, Engine, IndexSchema, Outcome, Query, Row};

use crate::clock::{Clock, SystemClock};
use crate::error::DocumentError;
use crate::id::{minted_at, IdError, IdSource, NodeId, Snowflake};
use crate::identity::{DocumentId, Ref, RevisionId};
use crate::query::{scan_between, scan_prefix};
use crate::record::{Amendable, Codes, Record, CODE_BASE};
use crate::revision::{Revision, Stored};
use crate::transaction::Transaction;
use crate::write;

/// A store and the mint that names what goes into it.
///
/// There is one clock here, inside the mint. ADR 0009 says the id and
/// `created_at` come from one read, and holding a second clock made that a
/// promise rather than a fact: two reads a few microseconds apart can land in
/// different milliseconds, so a revision could carry a timestamp its own id
/// disagreed with. The timestamp is read back out of the id instead.
pub struct Connection<E, C = SystemClock> {
    db: Db<E>,
    ids: Snowflake<C>,
}

impl<E: Engine> Connection<E, SystemClock> {
    /// Opens against the system clock and a node id nothing has to configure.
    ///
    /// See [`NodeId::derived`] for what that node id is and when to pin it.
    pub async fn open(engine: E, schemas: &[IndexSchema]) -> Result<Self, DocumentError> {
        Self::with_clock(engine, schemas, SystemClock, NodeId::derived()).await
    }
}

impl<E: Engine, C: Clock> Connection<E, C> {
    /// Opens against a clock and node of the caller's choosing, which is what a
    /// test uses to make two runs produce the same bytes.
    pub async fn with_clock(
        engine: E,
        schemas: &[IndexSchema],
        clock: C,
        node: NodeId,
    ) -> Result<Self, DocumentError> {
        Ok(Self {
            db: Db::open(engine, schemas).await?,
            ids: Snowflake::new(clock, node),
        })
    }

    /// The layer underneath, for the byte-level operations this one puts no
    /// seam over.
    pub fn db(&self) -> &Db<E> {
        &self.db
    }

    /// The next revision id, for a caller that is building the write itself.
    ///
    /// It reads the mint's clock once, which is what makes the id and the
    /// timestamp read back out of it agree by construction (ADR 0009).
    pub(crate) fn mint(&self) -> Result<RevisionId, IdError> {
        self.ids.next()
    }

    /// Files a new record.
    ///
    /// One operation: an absence check per unique index, the revision itself,
    /// and a row per index. A taken unique key refuses the whole thing, so
    /// nothing is half written.
    ///
    /// The first revision of a record is also its identity, so the document id
    /// and the revision id are the same number here and never again.
    ///
    /// The value is taken rather than borrowed. It moves into the revision that
    /// is filed and comes back as part of it, so the caller keeps what was
    /// stored without a copy being made of it on the way in.
    pub async fn insert<T: Record>(&self, value: T) -> Result<Stored<T>, DocumentError> {
        let revision = self.ids.next()?;
        let document = revision.opens();
        let envelope = Revision::new(value, document, minted_at(revision), None);

        let write = write::insert(&envelope, document, revision, Codes::at(CODE_BASE));
        match self.db.run(write.operation()).await? {
            Outcome::Committed(()) => Ok(Stored::new(revision, envelope)),
            Outcome::Aborted(code) => Err(write.refusal(code)),
        }
    }

    /// Stages several records and files them in one operation.
    ///
    /// [`Connection::insert`] and [`Connection::update`] are each one
    /// operation, which is the right unit for one record and the wrong one for
    /// a change that is only correct as a whole. A transaction composes the
    /// same writes into a single operation, so the store either holds all of
    /// them or none.
    pub fn transaction(&self) -> Transaction<'_, E, C> {
        Transaction::opening(self)
    }

    /// Supersedes a record with an edited copy of it.
    ///
    /// ADR 0001 already says this is a delete of the old index rows plus an
    /// insert of the new ones rather than a special operation, so that is
    /// literally what it builds. The old revision stays where it is and the new
    /// one names it.
    ///
    /// The unique check is placed after the old rows are retired, so it does
    /// not find this record's own key and call it a duplicate. That ordering is
    /// read-your-own-writes doing what ADR 0005 describes.
    ///
    /// The edit is taken rather than borrowed and moved out whole. The edited
    /// value goes into the revision that is filed and comes back as part of it,
    /// so nothing here is copied. A record edited through `DerefMut` carries the
    /// value it was read as, which is what names the rows to retire; one that
    /// was never edited names them with the value it still holds.
    ///
    /// The document id is the one thing that does not change, so a foreign key
    /// written against it keeps naming this record.
    ///
    /// A record the schema marked `#[immutable]` does not implement
    /// [`Amendable`] and so cannot be passed here at all. That is a compile
    /// error rather than a [`DocumentError`], because whether a record may be
    /// superseded is settled by the type and never by what the store holds.
    pub async fn update<T: Record + Amendable>(
        &self,
        stored: Stored<T>,
    ) -> Result<Stored<T>, DocumentError> {
        let (document, old, edited, now) = stored.into_parts();
        let new = self.ids.next()?;
        let envelope = Revision::new(now, document, minted_at(new), Some(old));
        let was: &T = edited.as_ref().unwrap_or(&envelope);

        let write = write::supersede(&envelope, was, document, old, new, Codes::at(CODE_BASE));
        match self.db.run(write.operation()).await? {
            Outcome::Committed(()) => Ok(Stored::new(new, envelope)),
            Outcome::Aborted(code) => Err(write.refusal(code)),
        }
    }

    /// Follows a reference to whatever revision of that record is live now.
    ///
    /// A reference names a record rather than a revision, so this is a read
    /// rather than a lookup in something the caller already holds: the answer
    /// is whichever revision is current when it is followed, which is the whole
    /// point of the id not moving when a record is edited.
    ///
    /// A reference to a record that is not there is [`DocumentError::NotFound`]
    /// rather than an empty answer, because a reference was written by somebody
    /// who had one.
    pub async fn load<T: Record>(&self, reference: Ref<T>) -> Result<Stored<T>, DocumentError> {
        let scan = scan_prefix::<2, 1>(&T::latest(), key![reference.id()]);
        self.select::<T>(scan, Some(1))
            .await?
            .pop()
            .ok_or(DocumentError::NotFound)
    }

    /// Follows many references at once, from one view of the store.
    ///
    /// The generated index is keyed on the record, so every reference is a row
    /// in one ordered index and the whole set is one scan between the lowest id
    /// asked for and the highest. That is one read rather than one per
    /// reference, and it is why this exists instead of a loop around
    /// [`Connection::load`].
    ///
    /// What it costs is reading the gap: ids between the two ends that nobody
    /// asked about come back and are dropped here. Snowflakes are minted in
    /// time order, so records written near each other sit near each other and
    /// the gap is small; a set drawn from opposite ends of a long-lived store
    /// is the case where a loop would read less.
    ///
    /// The answer is in index order, which is id order, and holds one entry per
    /// distinct reference that resolved. A reference naming nothing is missing
    /// from it rather than an error, because a set is not a promise that every
    /// member is there. Callers that need to pair them up index by
    /// [`Stored::id`].
    pub async fn load_many<T: Record>(
        &self,
        references: impl IntoIterator<Item = Ref<T>>,
    ) -> Result<Vec<Stored<T>>, DocumentError> {
        let mut wanted: Vec<DocumentId> = references.into_iter().map(Ref::id).collect();
        wanted.sort_unstable();
        wanted.dedup();

        let (Some(low), Some(high)) = (wanted.first(), wanted.last()) else {
            return Ok(Vec::new());
        };

        let scan = scan_between::<2, 1>(
            &T::latest(),
            Some((key![*low], Bound::Incl)),
            Some((key![*high], Bound::Incl)),
        );

        let mut found = self.select::<T>(scan, None).await?;
        found.retain(|stored| wanted.binary_search(&stored.id()).is_ok());
        Ok(found)
    }

    /// Every revision behind this one, newest first.
    ///
    /// Walking `prev` is the only thing that field is for. A chain that names a
    /// revision the store does not hold stops rather than failing, because a
    /// document is never deleted and a missing one means a store this handle
    /// cannot explain.
    pub async fn history<T: Record>(
        &self,
        stored: &Stored<T>,
    ) -> Result<Vec<Stored<T>>, DocumentError> {
        let mut older = Vec::new();
        let mut next = stored.prev();

        while let Some(revision) = next {
            let Some(envelope) = self.fetch::<T>(revision).await? else {
                break;
            };
            next = envelope.prev();
            older.push(Stored::new(revision, envelope));
        }

        Ok(older)
    }

    async fn fetch<T: Record>(
        &self,
        revision: RevisionId,
    ) -> Result<Option<Revision<T>>, DocumentError> {
        match self
            .db
            .run(&Query::object::<Revision<T>>(revision.0))
            .await?
        {
            Outcome::Committed(document) => Ok(document),
            Outcome::Aborted(code) => Err(DocumentError::Aborted(code)),
        }
    }

    /// Runs a scan the generated builder assembled, and shapes what comes back.
    pub(crate) async fn select<T: Record>(
        &self,
        scan: Query<Vec<Row>>,
        limit: Option<usize>,
    ) -> Result<Vec<Stored<T>>, DocumentError> {
        let scan = match limit {
            Some(rows) => scan.limit(rows),
            None => scan,
        };
        let query = scan.with_objects::<Revision<T>>();
        match self.db.run(&query).await? {
            Outcome::Committed(rows) => Ok(rows
                .into_iter()
                .map(|(row, envelope)| Stored::new(RevisionId(row.object), envelope))
                .collect()),
            Outcome::Aborted(code) => Err(DocumentError::Aborted(code)),
        }
    }
}
