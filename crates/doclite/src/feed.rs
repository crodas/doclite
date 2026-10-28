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
//!
//! [`Feed::watch`] is the same read with a tail: the rows already there, then
//! the ones that have not happened yet. A notice is published once the write
//! has committed, so a change made after the watch started is waiting rather
//! than raced for.
//!
//! ```
//! use doclite::{records, Change, Checkpoint, Connection, Document, DocumentError};
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
//! let mut changes = Deal::feed(&conn, "roma".into()).watch().await?;
//!
//! conn.insert(Deal { account: "roma".into(), amount_cents: 100 }).await?;
//!
//! match changes.next().await {
//!     Some(Ok(Change::Revised(deal))) => assert_eq!(deal.amount_cents, 100),
//!     Some(Ok(Change::Lagged { .. })) => unreachable!("nothing to fall behind"),
//!     Some(Err(why)) => return Err(why),
//!     None => unreachable!("the connection is still open"),
//! }
//!
//! assert!(
//!     changes.checkpoint() > Checkpoint::origin(),
//!     "what to come back with, past the change just handled"
//! );
//! # Ok(())
//! # }
//! # tokio::runtime::Builder::new_current_thread()
//! #     .build()
//! #     .expect("a current-thread runtime")
//! #     .block_on(example())
//! #     .expect("the example runs");
//! ```

use std::collections::{BTreeSet, VecDeque};
use std::marker::PhantomData;

use doclite_core::{Bound, Column, Engine, IndexId, Query, Range, RangeSpec, Row};

use crate::bus::{Notice, Subscription};
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

/// How many notices a subscriber may fall behind by before it is sent back to
/// the feed.
///
/// A number rather than an unbounded queue, because an unbounded one turns a
/// reader that stopped draining into memory the writer pays for. Falling behind
/// costs a scan and no notices, so the number only has to be large enough that
/// an ordinary reader never reaches it.
const BACKLOG: usize = 128;

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
    backlog: usize,
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
            backlog: BACKLOG,
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
    ///
    /// Ignored by [`Feed::watch`], which reads a whole catch-up at once: a page
    /// boundary there would put a live notice ahead of the rows still to come.
    pub fn limit(mut self, rows: usize) -> Self {
        self.limit = Some(rows);
        self
    }

    /// Which index this reads, for the gate and for a subscriber's filter.
    pub fn index(&self) -> IndexId {
        self.index
    }

    /// How many live notices may queue up for a watcher before it is told to
    /// read the rest out of the feed. Defaults to a number no ordinary reader
    /// reaches.
    pub fn backlog(mut self, notices: usize) -> Self {
        self.backlog = notices;
        self
    }

    /// Every change to this owner in the range, oldest first.
    ///
    /// The revision each row names is the one that made the change, so what
    /// comes back is the record as it was then rather than as it is now.
    /// Editing one is [`DocumentError::Stale`] unless it is still the live
    /// revision, which is the ordinary assertion doing its job.
    pub async fn get_many(self) -> Result<Vec<Stored<T>>, DocumentError> {
        self.page(self.since, self.limit).await
    }

    /// Everything from one cursor to this feed's high end, which is what both
    /// a plain read and a watcher catching up are.
    async fn page(
        &self,
        since: Checkpoint,
        limit: Option<usize>,
    ) -> Result<Vec<Stored<T>>, DocumentError> {
        let scan = (self.scan)(self.range_from(since));
        self.conn.select(scan, limit).await
    }

    /// Every change to this owner from the checkpoint on, waiting for the ones
    /// that have not happened yet.
    ///
    /// The order is the gate, then the subscription, then the scan, and each is
    /// forced. The gate first, or a notice could arrive from a feed whose older
    /// rows are still being filled. The subscription before the scan, or a
    /// write committing while the scan runs would reach neither half; with it
    /// first, such a change arrives as a notice whatever its revision, and the
    /// scan's own rows are remembered so the overlap is dropped rather than
    /// delivered twice.
    pub async fn watch(self) -> Result<Changes<'a, E, C, T>, DocumentError> {
        self.conn.ready(self.index).await?;
        let subscription = self
            .conn
            .subscribe(self.index, vec![self.owner.clone()], self.backlog);
        let caught_up = self.page(self.since, None).await?;

        Ok(Changes {
            cursor: self.since,
            seen: caught_up.iter().map(Stored::revision).collect(),
            page: caught_up.into(),
            feed: self,
            subscription,
            refill: false,
        })
    }

    /// The owner pinned, the revision ranged.
    ///
    /// An absent high end falls back to the owner alone rather than to the end
    /// of the index, for the reason [`crate::scan_within`] does the same: the
    /// low end is two components wide, and an unbounded high end would walk
    /// into every owner sorting above this one.
    fn range_from(&self, since: Checkpoint) -> RangeSpec {
        let lo = vec![self.owner.clone(), Column::encode(&since.revision())];
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

/// One thing that happened to the owner being watched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change<T> {
    /// A revision was filed: the record as it was at that change.
    Revised(Stored<T>),
    /// Notices were dropped because this reader was behind, and the ones it
    /// missed are being read out of the feed from here.
    ///
    /// Reported rather than hidden, so a reader that cares can say it fell
    /// behind. Nothing is lost either way: the rows are in the feed and the
    /// changes after this one are the ones it missed.
    Lagged {
        /// Where the catch-up resumed from.
        from: Checkpoint,
    },
}

/// One owner's changes, as they happen.
///
/// What [`Feed::watch`] answers. The rows the feed already held come first, in
/// order, and the stream then stays open for the ones that have not happened
/// yet. It borrows the connection, so a watcher lives inside the scope that
/// opened the store.
pub struct Changes<'a, E, C, T> {
    feed: Feed<'a, E, C, T>,
    subscription: Subscription,
    page: VecDeque<Stored<T>>,
    seen: BTreeSet<RevisionId>,
    cursor: Checkpoint,
    refill: bool,
}

impl<E: Engine, C: Clock, T: Record> Changes<'_, E, C, T> {
    /// The next change, waiting for one if the store has not had it yet.
    ///
    /// `None` only once the bus has let this subscriber go, which is what
    /// closing the connection does.
    pub async fn next(&mut self) -> Option<Result<Change<T>, DocumentError>> {
        loop {
            if let Some(stored) = self.page.pop_front() {
                self.cursor = Checkpoint::after(stored.revision());
                return Some(Ok(Change::Revised(stored)));
            }

            if self.refill {
                self.refill = false;
                match self.feed.page(self.cursor, None).await {
                    Ok(missed) => {
                        self.seen = missed.iter().map(Stored::revision).collect();
                        self.page = missed.into();
                        continue;
                    }
                    Err(why) => return Some(Err(why)),
                }
            }

            if self.subscription.lagged() {
                self.refill = true;
                return Some(Ok(Change::Lagged { from: self.cursor }));
            }

            let notice = self.subscription.notice().await?;
            if self.seen.remove(&notice.revision) {
                continue;
            }
            match self.read(&notice).await {
                Ok(stored) => {
                    self.cursor = Checkpoint::after(stored.revision());
                    return Some(Ok(Change::Revised(stored)));
                }
                Err(why) => return Some(Err(why)),
            }
        }
    }

    /// Where a reader that stops here resumes from.
    ///
    /// Just past the last change handed over, which is not the same as just
    /// past the last one read: rows caught up but not yet delivered are still
    /// ahead of this, so a reader that stops mid-stream and comes back with it
    /// loses nothing.
    pub fn checkpoint(&self) -> Checkpoint {
        self.cursor
    }

    /// The revision a notice names, read back out of the store.
    ///
    /// A notice carries ids rather than bytes, so this is one point read. An
    /// object is never deleted, so a notice naming one the store does not hold
    /// means a store nothing here can explain.
    async fn read(&self, notice: &Notice) -> Result<Stored<T>, DocumentError> {
        let envelope = self
            .feed
            .conn
            .fetch::<T>(notice.revision)
            .await?
            .ok_or(DocumentError::NotFound)?;
        Ok(Stored::new(notice.revision, envelope))
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
