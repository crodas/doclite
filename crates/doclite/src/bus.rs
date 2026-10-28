//! Who to tell once a write has committed.
//!
//! A feed row is durable, so a live notice is an optimisation over reading one:
//! a subscriber that misses one catches up by scanning from its own checkpoint.
//! That is what lets everything here be best effort, and in particular what
//! lets a full channel drop a notice rather than hold up a writer.
//!
//! Nothing here interprets a key. A notice carries the columns the record's own
//! walk produced and a subscriber matches a prefix of them, which is the same
//! split the store makes: bytes down here, meaning one layer up.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use doclite_core::{Column, IndexId};
use futures_channel::mpsc::{channel, Receiver, Sender};
use futures_core::Stream;

use crate::identity::{DocumentId, RevisionId};

/// One feed row, as the write that filed it saw it.
///
/// The document and the revision are carried rather than read back out of the
/// key, because the layer that publishes one holds both already and a key is
/// bytes once it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) index: IndexId,
    pub(crate) key: Vec<Column>,
    pub(crate) document: DocumentId,
    pub(crate) revision: RevisionId,
}

/// Every live subscriber, and a count of them that can be read without the
/// lock.
///
/// The count is what keeps a store nobody is watching from paying for this at
/// all: a write asks [`Bus::quiet`] and walks nothing when the answer is yes,
/// which is `Gate::armed`'s trick in a second place.
#[derive(Default)]
pub(crate) struct Bus {
    subscribers: Mutex<Vec<Subscriber>>,
    live: AtomicUsize,
}

struct Subscriber {
    index: IndexId,
    prefix: Vec<Column>,
    notices: Sender<Notice>,
    lagged: Arc<AtomicBool>,
}

/// One subscriber's end of the bus.
///
/// Dropping it is how a subscription ends: the next publish finds the channel
/// disconnected and forgets it.
pub(crate) struct Subscription {
    notices: Receiver<Notice>,
    lagged: Arc<AtomicBool>,
}

impl Subscription {
    /// The next notice, or `None` once the bus has let this subscriber go.
    ///
    /// Written out rather than reached through `StreamExt::next`, so waiting
    /// for a notice needs no runtime and no futures-util: ADR 0006 keeps both
    /// the caller's.
    pub(crate) fn notice(&mut self) -> impl Future<Output = Option<Notice>> + '_ {
        Waiting(&mut self.notices)
    }

    /// Whether a notice was dropped because this subscriber was behind, which
    /// clears the flag: saying so once is what sends the reader back to the
    /// durable feed.
    pub(crate) fn lagged(&self) -> bool {
        self.lagged.swap(false, Ordering::AcqRel)
    }
}

/// One poll of a subscriber's channel, as a future.
struct Waiting<'a>(&'a mut Receiver<Notice>);

impl Future for Waiting<'_> {
    type Output = Option<Notice>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Notice>> {
        Pin::new(&mut *self.0).poll_next(cx)
    }
}

impl Bus {
    /// Whether nobody is listening, read without taking the lock because the
    /// answer is yes on every write of an ordinary store.
    pub(crate) fn quiet(&self) -> bool {
        self.live.load(Ordering::Relaxed) == 0
    }

    /// Listens for notices from one index whose key starts with these columns.
    ///
    /// The capacity is the caller's, because what a sensible one is depends on
    /// how fast that subscriber drains and nothing here can know. Overflowing
    /// it is not an error: the reader is told it fell behind and reads the rest
    /// out of the feed. What a channel holds is this plus the slot every sender
    /// is guaranteed, so a capacity of zero still delivers one notice.
    pub(crate) fn subscribe(
        &self,
        index: IndexId,
        prefix: Vec<Column>,
        capacity: usize,
    ) -> Subscription {
        let (notices, receiving) = channel(capacity);
        let lagged = Arc::new(AtomicBool::new(false));

        let mut held = self.held();
        held.push(Subscriber {
            index,
            prefix,
            notices,
            lagged: Arc::clone(&lagged),
        });
        self.live.store(held.len(), Ordering::Relaxed);

        Subscription {
            notices: receiving,
            lagged,
        }
    }

    /// Hands every notice to every subscriber it belongs to, dropping those
    /// whose reader has gone.
    ///
    /// Called after the write committed and never during it: an operation is
    /// re-walked when it loses a race (ADR 0003), so a notice published from
    /// inside the walk would announce a change that never happened.
    pub(crate) fn publish(&self, notices: &[Notice]) {
        if notices.is_empty() {
            return;
        }

        let mut held = self.held();
        held.retain_mut(|subscriber| subscriber.told(notices));
        self.live.store(held.len(), Ordering::Relaxed);
    }

    /// A poisoned lock is taken anyway, because a panic cannot leave a
    /// `Vec<Subscriber>` in a state the next publish would misread, and
    /// refusing to publish would be a notice lost for the sake of tidiness.
    fn held(&self) -> MutexGuard<'_, Vec<Subscriber>> {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl Subscriber {
    /// Whether this notice is in the index this subscriber reads, under a key
    /// that starts with the one it asked for.
    fn wants(&self, notice: &Notice) -> bool {
        notice.index == self.index && notice.key.starts_with(&self.prefix)
    }

    /// Hands over every notice this subscriber wants, and answers whether it is
    /// still there afterwards.
    fn told(&mut self, notices: &[Notice]) -> bool {
        for notice in notices {
            if self.wants(notice) && !self.send(notice.clone()) {
                return false;
            }
        }
        true
    }

    /// Whether to keep this subscriber: a full channel is a reader that fell
    /// behind, which is told and kept, and a closed one is a reader that has
    /// gone.
    fn send(&mut self, notice: Notice) -> bool {
        match self.notices.try_send(notice) {
            Ok(()) => true,
            Err(refused) if refused.is_full() => {
                self.lagged.store(true, Ordering::Release);
                true
            }
            Err(_gone) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use doclite_core::ObjectId;

    use super::*;

    const FEED: IndexId = IndexId("deal_feed");
    const OTHER: IndexId = IndexId("note_feed");

    fn owner(name: &str) -> Column {
        Column::encode(name)
    }

    fn notice(index: IndexId, name: &str, revision: u64) -> Notice {
        Notice {
            index,
            key: vec![owner(name), Column::encode(&RevisionId(ObjectId(revision)))],
            document: DocumentId(ObjectId(revision)),
            revision: RevisionId(ObjectId(revision)),
        }
    }

    /// Every notice already queued, which is what this reads without a runtime.
    fn drain(subscription: &mut Subscription) -> Vec<u64> {
        let mut seen = Vec::new();
        while let Ok(notice) = subscription.notices.try_recv() {
            seen.push(notice.revision.0 .0);
        }
        seen
    }

    /// The awaiting half, which the watch stream is built on.
    #[tokio::test]
    async fn a_notice_published_after_a_subscriber_waits_wakes_it() {
        let bus = Bus::default();
        let mut roma = bus.subscribe(FEED, vec![owner("roma")], 4);

        bus.publish(&[notice(FEED, "roma", 7)]);

        let heard = roma.notice().await.expect("a notice was published");
        assert_eq!(heard.revision, RevisionId(ObjectId(7)));
    }

    #[test]
    fn a_store_nobody_watches_is_quiet() {
        let bus = Bus::default();
        assert!(bus.quiet());

        let subscription = bus.subscribe(FEED, vec![owner("roma")], 4);
        assert!(!bus.quiet());

        drop(subscription);
        bus.publish(&[notice(FEED, "roma", 1)]);
        assert!(bus.quiet(), "a reader that has gone is forgotten");
    }

    #[test]
    fn a_subscriber_hears_only_its_own_owner_and_its_own_index() {
        let bus = Bus::default();
        let mut roma = bus.subscribe(FEED, vec![owner("roma")], 8);

        bus.publish(&[
            notice(FEED, "roma", 1),
            notice(FEED, "sur", 2),
            notice(OTHER, "roma", 3),
            notice(FEED, "roma", 4),
        ]);

        assert_eq!(drain(&mut roma), [1, 4]);
    }

    #[test]
    fn two_subscribers_of_one_owner_both_hear_it() {
        let bus = Bus::default();
        let mut one = bus.subscribe(FEED, vec![owner("roma")], 4);
        let mut two = bus.subscribe(FEED, vec![owner("roma")], 4);

        bus.publish(&[notice(FEED, "roma", 1)]);

        assert_eq!(drain(&mut one), [1]);
        assert_eq!(drain(&mut two), [1]);
    }

    /// A reader that is behind is told rather than dropped, because the rows it
    /// missed are still in the feed and the flag is what sends it back there.
    #[test]
    fn a_full_channel_is_a_lag_rather_than_a_lost_subscriber() {
        let bus = Bus::default();
        let mut slow = bus.subscribe(FEED, vec![owner("roma")], 1);

        for revision in 1..=4 {
            bus.publish(&[notice(FEED, "roma", revision)]);
        }

        assert!(slow.lagged(), "the notices past the channel did not fit");
        assert!(!slow.lagged(), "and saying so once is enough");
        assert!(!bus.quiet(), "the subscriber is still there");

        let heard = drain(&mut slow);
        assert!(!heard.is_empty() && heard.len() < 4, "{heard:?}");
        assert_eq!(
            heard,
            (1..=heard.len() as u64).collect::<Vec<u64>>(),
            "what did fit is the oldest, in order"
        );
    }

    /// An empty key is every key, which is what a subscriber to a whole index
    /// would ask for.
    #[test]
    fn a_prefix_of_nothing_matches_every_row_of_the_index() {
        let bus = Bus::default();
        let mut all = bus.subscribe(FEED, Vec::new(), 8);

        bus.publish(&[notice(FEED, "roma", 1), notice(FEED, "sur", 2)]);

        assert_eq!(drain(&mut all), [1, 2]);
    }
}
