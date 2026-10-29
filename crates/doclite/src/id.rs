//! Snowflake ids, minted here because the storage layer will not.
//!
//! ADR 0007 keeps identity out of `doclite-core`: an operation has to be fully
//! described before it runs, so the store can never invent a value mid-batch.
//! This is the layer that knows what a revision is, so this is where an id
//! comes from.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use doclite_core::ObjectId;

use crate::clock::{Clock, Timestamp};
use crate::identity::RevisionId;

/// Milliseconds from the Unix epoch to 2026-01-01T00:00:00Z.
///
/// Counting from here rather than 1970 is what buys the layout its lifetime:
/// 41 bits of milliseconds is about 69 years, which from 1970 would have run
/// out in 2039. A test pins this constant to the date rather than trusting the
/// arithmetic.
pub const EPOCH_MS: u64 = 1_767_225_600_000;

const NODE_BITS: u32 = 10;
const SEQUENCE_BITS: u32 = 12;
const MAX_NODE: u16 = (1 << NODE_BITS) - 1;
const MAX_SEQUENCE: u16 = (1 << SEQUENCE_BITS) - 1;
const MAX_MILLIS: u64 = (1 << 41) - 1;

/// Which writer minted an id.
///
/// Ten bits, so two writers sharing one mint colliding ids. A collision is an
/// insert refused with `StorageError::DuplicateObject` rather than a silent
/// overwrite, which is ADR 0001 doing the work, but it is still a lost write to
/// whoever lost the race.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u16);

impl NodeId {
    /// Refuses anything the ten bits cannot hold.
    pub fn new(node: u16) -> Result<Self, IdError> {
        if node > MAX_NODE {
            return Err(IdError::NodeOutOfRange(node));
        }
        Ok(Self(node))
    }

    /// The node id nothing has to configure.
    ///
    /// `DOCLITE_NODE_ID` wins when it is set, so a deployment can pin a node
    /// without giving up the zero-configuration default. Unset, the hostname
    /// and process id are hashed into the ten bits, which two writers collide
    /// on about once in a thousand. Pin it in production.
    ///
    /// Set but unreadable is refused rather than derived around. A deployment
    /// that wrote the variable meant to pin a node, and a typo that fell back
    /// to the hash would leave it colliding at exactly the odds it had set out
    /// to remove, with nothing said.
    pub fn derived() -> Result<Self, IdError> {
        match std::env::var("DOCLITE_NODE_ID") {
            Ok(pinned) => Self::pinned(&pinned),
            Err(_) => Ok(Self::hashed()),
        }
    }

    /// What one value of `DOCLITE_NODE_ID` means, separated from reading it so
    /// the refusals are testable without a process-wide environment.
    fn pinned(value: &str) -> Result<Self, IdError> {
        let node: u16 = value
            .trim()
            .parse()
            .map_err(|_| IdError::PinnedNodeUnreadable)?;
        Self::new(node)
    }

    /// The ten bits a hostname and a process id hash into.
    fn hashed() -> Self {
        let mut hasher = DefaultHasher::new();
        std::env::var("HOSTNAME")
            .unwrap_or_default()
            .hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        Self((hasher.finish() as u16) & MAX_NODE)
    }

    /// The ten bits themselves, for a caller that has to record which node it
    /// is running as.
    pub fn get(&self) -> u16 {
        self.0
    }
}

/// What stops an id from being minted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdError {
    /// A node id wider than the ten bits the layout gives it.
    #[error("a node id is ten bits, given {0}")]
    NodeOutOfRange(u16),

    /// The machine's clock is set before the epoch ids are counted from.
    #[error("the clock reads before the DocLite epoch")]
    BeforeEpoch,

    /// The timestamp no longer fits, which is the layout running out rather
    /// than the clock being wrong.
    #[error("the clock has run past what 41 bits of milliseconds can hold")]
    PastTheEnd,

    /// Refused rather than absorbed, because an id that went backwards would
    /// put a later revision earlier in every id-ordered scan.
    #[error("the clock went backwards, from {was} to {now}")]
    WentBackwards {
        /// The last millisecond an id was minted in.
        was: u64,
        /// What the clock reads now.
        now: u64,
    },

    /// More ids asked for in one millisecond than the sequence bits hold.
    #[error("all 4096 ids for this millisecond are spent")]
    Exhausted,

    /// `DOCLITE_NODE_ID` is set to something that is not a node id.
    ///
    /// Refused rather than derived around, because a deployment that set it
    /// meant to pin a node, and silently hashing one instead would leave it
    /// colliding exactly where it thought it had stopped.
    #[error("DOCLITE_NODE_ID is set to something that is not a number")]
    PinnedNodeUnreadable,
}

/// What mints an id.
pub trait IdSource: Send + Sync {
    /// Takes `&self` so a mint can be shared, which is what lets a connection
    /// hold one.
    fn next(&self) -> Result<RevisionId, IdError>;
}

/// A time-ordered id: 41 bits of milliseconds, 10 of node, 12 of sequence.
///
/// The sign bit is left zero so an id survives a round trip through anything
/// that reads it as an `i64`. Ordering by id is ordering by creation, which is
/// what makes a revision chain and any id-keyed scan read in the order things
/// happened.
pub struct Snowflake<C> {
    clock: C,
    node: NodeId,
    last: Mutex<(u64, u16)>,
}

impl<C: Clock> Snowflake<C> {
    /// Takes the clock rather than reading the system one, so a test can pin
    /// what an id says about when it was minted.
    pub fn new(clock: C, node: NodeId) -> Self {
        Self {
            clock,
            node,
            last: Mutex::new((0, 0)),
        }
    }
}

impl<C: Clock> IdSource for Snowflake<C> {
    /// A spent millisecond is an error rather than a spin. This is called while
    /// an operation is being built, and spinning would block whatever runtime
    /// the caller is on for something 4096 ids per millisecond makes unlikely.
    fn next(&self) -> Result<RevisionId, IdError> {
        let Timestamp(now) = self.clock.now();
        let millis = now.checked_sub(EPOCH_MS).ok_or(IdError::BeforeEpoch)?;
        if millis > MAX_MILLIS {
            return Err(IdError::PastTheEnd);
        }

        let mut last = self.last.lock().map_err(|_| IdError::Exhausted)?;
        let (was, sequence) = *last;

        let sequence = if millis > was {
            0
        } else if millis == was {
            if sequence >= MAX_SEQUENCE {
                return Err(IdError::Exhausted);
            }
            sequence + 1
        } else {
            return Err(IdError::WentBackwards { was, now: millis });
        };

        *last = (millis, sequence);
        drop(last);

        let id = (millis << (NODE_BITS + SEQUENCE_BITS))
            | (u64::from(self.node.get()) << SEQUENCE_BITS)
            | u64::from(sequence);
        Ok(RevisionId(ObjectId(id)))
    }
}

/// When an id was minted, back out of the id itself.
pub fn minted_at(id: RevisionId) -> Timestamp {
    Timestamp((id.0 .0 >> (NODE_BITS + SEQUENCE_BITS)) + EPOCH_MS)
}

/// The lowest id that could have been minted in this millisecond.
///
/// The inverse of [`minted_at`], rounding down: a caller holding a plain
/// timestamp rather than an id gets the start of that millisecond, so a scan
/// bounded by one replays the ids already inside it rather than skipping them.
/// Saturating at both ends for the reason [`SystemClock::now`] saturates, since
/// a clock reading outside the layout means a machine nothing can fix here.
///
/// [`SystemClock::now`]: crate::SystemClock::now
pub fn first_minted_at(at: Timestamp) -> RevisionId {
    let millis = at.0.saturating_sub(EPOCH_MS).min(MAX_MILLIS);
    RevisionId(ObjectId(millis << (NODE_BITS + SEQUENCE_BITS)))
}

/// Which node minted an id.
pub fn minted_by(id: RevisionId) -> NodeId {
    NodeId(((id.0 .0 >> SEQUENCE_BITS) as u16) & MAX_NODE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    /// A deployment that wrote the variable meant to pin a node. Falling back
    /// to the hash would leave it colliding at exactly the odds it set out to
    /// remove, so an unreadable value is refused instead.
    #[test]
    fn a_pinned_node_that_is_not_a_number_is_refused() {
        assert_eq!(NodeId::pinned("one"), Err(IdError::PinnedNodeUnreadable));
        assert_eq!(NodeId::pinned(""), Err(IdError::PinnedNodeUnreadable));
        assert_eq!(NodeId::pinned("-1"), Err(IdError::PinnedNodeUnreadable));
    }

    /// Out of range is the error that already existed, carrying the number, so
    /// an operator reads what they wrote rather than that it was unreadable.
    #[test]
    fn a_pinned_node_too_wide_for_ten_bits_names_the_number() {
        assert_eq!(NodeId::pinned("1024"), Err(IdError::NodeOutOfRange(1024)));
        assert_eq!(NodeId::pinned(" 1023 ").map(|node| node.get()), Ok(1023));
    }

    /// The epoch constant is load-bearing for the id's lifetime, so it is
    /// checked against the date rather than trusted.
    #[test]
    fn the_epoch_is_the_first_of_january_2026() {
        let days_since_1970 = (1970..2026)
            .map(|year| {
                if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                    366
                } else {
                    365
                }
            })
            .sum::<u64>();
        assert_eq!(EPOCH_MS, days_since_1970 * 24 * 60 * 60 * 1000);
    }

    #[test]
    fn forty_one_bits_of_milliseconds_reaches_past_2090() {
        let years = (MAX_MILLIS / 1000 / 60 / 60 / 24) / 365;
        assert!(years > 64, "{years} years is not enough runway");
    }

    fn at(millis: u64) -> Snowflake<FixedClock> {
        Snowflake::new(
            FixedClock(Timestamp(EPOCH_MS + millis)),
            NodeId::new(3).expect("three fits in ten bits"),
        )
    }

    #[test]
    fn an_id_carries_back_the_time_and_node_that_made_it() {
        let id = at(1234).next().expect("the first id of a millisecond");
        assert_eq!(minted_at(id), Timestamp(EPOCH_MS + 1234));
        assert_eq!(minted_by(id).get(), 3);
    }

    #[test]
    fn the_sign_bit_stays_clear() {
        let id = at(MAX_MILLIS).next().expect("the last millisecond");
        assert!(i64::try_from(id.0 .0).is_ok());
    }

    #[test]
    fn ids_from_one_millisecond_ascend() {
        let mint = at(1);
        let ids: Vec<u64> = (0..64)
            .map(|_| mint.next().expect("well inside one millisecond").0 .0)
            .collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn a_later_millisecond_sorts_after_an_earlier_one() {
        let early = at(1).next().expect("an id");
        let late = at(2).next().expect("an id");
        assert!(early < late);
    }

    #[test]
    fn a_spent_millisecond_refuses_rather_than_spinning() {
        let mint = at(1);
        for _ in 0..=MAX_SEQUENCE {
            mint.next().expect("the sequence has room");
        }
        assert_eq!(mint.next(), Err(IdError::Exhausted));
    }

    #[test]
    fn a_clock_that_went_backwards_refuses() {
        let mint = Snowflake::new(
            FixedClock(Timestamp(EPOCH_MS + 10)),
            NodeId::new(0).expect("zero fits"),
        );
        mint.next().expect("the first id");
        let back = Snowflake::new(
            FixedClock(Timestamp(EPOCH_MS + 5)),
            NodeId::new(0).expect("zero fits"),
        );
        *back.last.lock().expect("a fresh lock") = (10, 0);
        assert_eq!(back.next(), Err(IdError::WentBackwards { was: 10, now: 5 }));
    }

    #[test]
    fn a_clock_before_the_epoch_refuses() {
        let mint = Snowflake::new(
            FixedClock(Timestamp(EPOCH_MS - 1)),
            NodeId::new(0).expect("zero fits"),
        );
        assert_eq!(mint.next(), Err(IdError::BeforeEpoch));
    }

    /// A timestamp is a usable cursor because this is its id, and the two agree
    /// about the millisecond.
    #[test]
    fn a_timestamp_becomes_the_first_id_of_its_millisecond() {
        let at = Timestamp(EPOCH_MS + 1234);
        let first = first_minted_at(at);
        assert_eq!(minted_at(first), at);
        assert_eq!(minted_by(first).get(), 0);
    }

    /// Rounding down is what makes a timestamp cursor replay rather than skip:
    /// every id minted in that millisecond sorts at or after it.
    #[test]
    fn the_first_id_of_a_millisecond_sorts_before_every_id_in_it() {
        let mint = at(1234);
        let first = first_minted_at(Timestamp(EPOCH_MS + 1234));
        for _ in 0..8 {
            assert!(first <= mint.next().expect("the sequence has room"));
        }
    }

    #[test]
    fn a_timestamp_outside_the_layout_saturates_rather_than_wrapping() {
        assert_eq!(
            first_minted_at(Timestamp(0)),
            first_minted_at(Timestamp(EPOCH_MS))
        );
        let last = first_minted_at(Timestamp(u64::MAX));
        assert_eq!(minted_at(last), Timestamp(EPOCH_MS + MAX_MILLIS));
        assert!(i64::try_from(last.0 .0).is_ok(), "the sign bit stays clear");
    }

    #[test]
    fn a_node_id_past_ten_bits_is_refused() {
        assert_eq!(NodeId::new(1024), Err(IdError::NodeOutOfRange(1024)));
        assert!(NodeId::new(1023).is_ok());
    }

    /// The hash is masked to the ten bits, so the derived half can never be
    /// the out-of-range refusal.
    #[test]
    fn a_hashed_node_id_always_fits() {
        assert!(NodeId::hashed().get() <= MAX_NODE);
    }
}
