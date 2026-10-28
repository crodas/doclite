//! Reading what happened to one owner, in the order it happened.
//!
//! A feed is an index (`#[owner]` declares it) whose rows accumulate: one per
//! revision, keyed on the owner, the revision and the document. So catching up
//! is a range scan of one ordered index, which is why nothing below this file
//! needed a new idea to support it.
//!
//! What a caller holds between reads is a [`Checkpoint`]. A revision id is a
//! snowflake, which is a timestamp with the ties broken, so a plain timestamp
//! converts into one and the scan is the same either way.
//!
//! ```
//! use doclite::{records, Checkpoint, Connection, Document, DocumentError};
//! use doclite_memory::MemoryEngine;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Clone, Serialize, Deserialize, Document)]
//! struct Deal {
//!     #[owner]
//!     account: String,
//!     amount_cents: i64,
//! }
//!
//! # async fn example() -> Result<(), DocumentError> {
//! let conn = Connection::open(MemoryEngine::new(), records![Deal]).await?;
//!
//! let mut deal = conn
//!     .insert(Deal { account: "roma".into(), amount_cents: 100 })
//!     .await?;
//! deal.amount_cents = 200;
//! let deal = conn.update(deal).await?;
//!
//! let changes = Deal::feed(&conn, "roma".into()).get_many().await?;
//! assert_eq!(
//!     changes.iter().map(|change| change.amount_cents).collect::<Vec<_>>(),
//!     [100, 200],
//!     "what the record held at each change, oldest first"
//! );
//!
//! let caught_up = Deal::feed(&conn, "roma".into())
//!     .since(Checkpoint::after(deal.revision()))
//!     .get_many()
//!     .await?;
//! assert!(caught_up.is_empty(), "nothing has happened since");
//! # Ok(())
//! # }
//! # tokio::runtime::Builder::new_current_thread()
//! #     .build()
//! #     .expect("a current-thread runtime")
//! #     .block_on(example())
//! #     .expect("the example runs");
//! ```

use std::marker::PhantomData;

use doclite_core::{Bound, Column, Engine, IndexId, Query, Range, RangeSpec, Row};

use crate::clock::{Clock, Timestamp};
use crate::connection::Connection;
use crate::error::DocumentError;
use crate::id::first_minted_at;
use crate::identity::RevisionId;
use crate::record::Record;
use crate::revision::Stored;

/// Where a reader of a feed left off.
///
/// A revision rather than a time, because two revisions can share a
/// millisecond and a cursor that cannot tell them apart either skips one or
/// repeats the pair for ever. A [`Timestamp`] still converts in, landing at the
/// start of its millisecond, so a caller that only kept the time replays a few
/// rows rather than missing any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Checkpoint(RevisionId);

impl Checkpoint {
    /// Before anything was written, which is what a reader with no checkpoint
    /// starts from.
    pub fn origin() -> Self {
        Self(RevisionId(doclite_core::ObjectId(0)))
    }

    /// Starting at this revision, which is included.
    pub fn at(revision: RevisionId) -> Self {
        Self(revision)
    }

    /// Starting just past this revision, for a reader resuming from the last
    /// one it handled.
    ///
    /// The id space is dense, so the next id is the next integer; there is no
    /// revision between the two.
    pub fn after(revision: RevisionId) -> Self {
        Self(RevisionId(doclite_core::ObjectId(
            revision.0 .0.saturating_add(1),
        )))
    }

    /// The revision this cursor sits at.
    pub fn revision(self) -> RevisionId {
        self.0
    }

    /// When that revision was minted, read back out of the id itself.
    pub fn when(self) -> Timestamp {
        crate::id::minted_at(self.0)
    }
}

impl From<RevisionId> for Checkpoint {
    fn from(revision: RevisionId) -> Self {
        Self::at(revision)
    }
}

/// Rounds down to the first id the millisecond could hold, so a cursor built
/// from a time replays rather than skips.
impl From<Timestamp> for Checkpoint {
    fn from(at: Timestamp) -> Self {
        Self::at(first_minted_at(at))
    }
}

/// How the generated code hands over the one thing this file cannot write
/// itself.
///
/// A scan needs the index's arity at compile time and this is generic over the
/// record rather than over a width, so the range is built here as plain data
/// and turned into a query by the derive, which knows the arity. A plain
/// function pointer rather than a closure, because it captures nothing and a
/// re-scan needs it again later.
pub type Scan = fn(RangeSpec) -> Query<Vec<Row>>;

/// One owner's changes, as a scan being assembled.
///
/// `#[derive(Document)]` writes the method that builds one, so a caller reaches
/// this through `<Record>::feed(&conn, owner)` rather than naming it.
pub struct Feed<'a, E, C, T> {
    conn: &'a Connection<E, C>,
    index: IndexId,
    owner: Column,
    scan: Scan,
    since: Checkpoint,
    until: Option<Checkpoint>,
    limit: Option<usize>,
    record: PhantomData<fn() -> T>,
}

impl<'a, E: Engine, C: Clock, T: Record> Feed<'a, E, C, T> {
    /// Called by the generated `feed` method with the owner's column and the
    /// index it belongs to.
    pub fn new(conn: &'a Connection<E, C>, index: IndexId, owner: Column, scan: Scan) -> Self {
        Self {
            conn,
            index,
            owner,
            scan,
            since: Checkpoint::origin(),
            until: None,
            limit: None,
            record: PhantomData,
        }
    }

    /// Where to start, inclusive.
    ///
    /// Inclusive because a cursor built from a timestamp has to replay its
    /// millisecond rather than skip it. A reader resuming from a revision it
    /// has already handled says [`Checkpoint::after`].
    pub fn since(mut self, from: impl Into<Checkpoint>) -> Self {
        self.since = from.into();
        self
    }

    /// Where to stop, inclusive. Absent means the end of this owner's rows.
    pub fn until(mut self, to: impl Into<Checkpoint>) -> Self {
        self.until = Some(to.into());
        self
    }

    /// Rows to read at most, which is how a long feed is paged: take the last
    /// revision of one page and [`Checkpoint::after`] it for the next.
    pub fn limit(mut self, rows: usize) -> Self {
        self.limit = Some(rows);
        self
    }

    /// Which index this reads, for the gate and for a subscriber's filter.
    pub fn index(&self) -> IndexId {
        self.index
    }

    /// Every change to this owner in the range, oldest first.
    ///
    /// The revision each row names is the one that made the change, so what
    /// comes back is the record as it was then rather than as it is now.
    /// Editing one is [`DocumentError::Stale`] unless it is still the live
    /// revision, which is the ordinary assertion doing its job.
    pub async fn get_many(self) -> Result<Vec<Stored<T>>, DocumentError> {
        let scan = (self.scan)(self.range());
        self.conn.select(scan, self.limit).await
    }

    /// The owner pinned, the revision ranged.
    ///
    /// An absent high end falls back to the owner alone rather than to the end
    /// of the index, for the reason [`crate::scan_within`] does the same: the
    /// low end is two components wide, and an unbounded high end would walk
    /// into every owner sorting above this one.
    fn range(&self) -> RangeSpec {
        let lo = vec![self.owner.clone(), Column::encode(&self.since.revision())];
        let hi = match self.until {
            Some(until) => vec![self.owner.clone(), Column::encode(&until.revision())],
            None => vec![self.owner.clone()],
        };
        RangeSpec::Between {
            lo: Some((lo, Bound::Incl)),
            hi: Some((hi, Bound::Incl)),
        }
    }
}

/// The range a feed scan is, for the generated code to wrap in its own index.
///
/// Public so the expansion can name it; a caller has no use for it.
pub fn scanning<const N: usize>(
    index: &doclite_core::Index<N>,
    spec: RangeSpec,
) -> Query<Vec<Row>> {
    Query::scan(index, Range::<N, N>::from_spec(spec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use doclite_core::ObjectId;

    fn revision(id: u64) -> RevisionId {
        RevisionId(ObjectId(id))
    }

    #[test]
    fn a_checkpoint_at_a_revision_includes_it_and_after_it_does_not() {
        assert_eq!(Checkpoint::at(revision(7)).revision(), revision(7));
        assert_eq!(Checkpoint::after(revision(7)).revision(), revision(8));
    }

    /// The last id is its own successor rather than wrapping to the first,
    /// which would hand a reader the whole feed again.
    #[test]
    fn a_checkpoint_after_the_last_possible_revision_stays_there() {
        let last = revision(u64::MAX);
        assert_eq!(Checkpoint::after(last).revision(), last);
    }

    /// A time is a usable cursor because it lands at the bottom of its
    /// millisecond, so every revision minted in it is still ahead.
    #[test]
    fn a_timestamp_checkpoint_sits_at_the_start_of_its_millisecond() {
        let at = Timestamp(crate::id::EPOCH_MS + 5);
        let from = Checkpoint::from(at);
        assert_eq!(from.when(), at);
        assert!(from.revision() <= Checkpoint::at(first_minted_at(at)).revision());
    }

    #[test]
    fn the_origin_is_before_every_revision() {
        assert!(Checkpoint::origin() < Checkpoint::at(revision(1)));
    }
}
