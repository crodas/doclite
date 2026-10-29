//! Where a document's timestamp comes from.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use doclite_core::{Column, ColumnType, IntoColumn, ToOrdBytes};
use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch.
///
/// A plain integer rather than a date type, because the store only ever
/// compares and stores it. A caller that wants calendar arithmetic converts at
/// its own edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timestamp(pub u64);

/// A record holds one in a field and files it in an index, so it is a column
/// like any other. Ordering is the integer's, which is chronological.
impl ToOrdBytes for Timestamp {
    const COLUMN_TYPE: ColumnType = <u64 as ToOrdBytes>::COLUMN_TYPE;

    fn to_ord_bytes(&self) -> Vec<u8> {
        self.0.to_ord_bytes()
    }
}

impl IntoColumn for Timestamp {
    fn into_column(self) -> Column {
        Column::encode(&self)
    }
}

/// Where `now` comes from.
///
/// A trait rather than a direct call to the system clock so a test can pin
/// time. That matters more than it looks: [`crate::Connection`] reads the clock
/// once while building an operation, never during the walk, because `Db::run`
/// re-walks on conflict and a clock read inside the walk would give a different
/// timestamp on every attempt.
pub trait Clock: Send + Sync {
    /// Infallible, so an insert does not become fallible for a case that means
    /// the machine's clock is unusable anyway.
    fn now(&self) -> Timestamp;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    /// A clock reading before the Unix epoch is reported as the epoch. The
    /// alternative is a fallible `now`, which would make every insert fallible
    /// for a case that means the machine's clock is unusable anyway.
    fn now(&self) -> Timestamp {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis())
            .unwrap_or(0);
        Timestamp(u64::try_from(millis).unwrap_or(u64::MAX))
    }
}

/// A clock that does not move, so two runs of the same work produce the same
/// bytes.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// A clock a caller moves by hand, for work that has to happen at more than one
/// time.
///
/// [`FixedClock`] pins an instant, which is what makes a run reproducible and
/// what makes a deadline untestable: everything computed against it is the same
/// distance away for ever. This one holds the instant somewhere both halves can
/// reach, so a test steps time rather than pinning it.
///
/// A clone shares the instant rather than copying it. That is the whole point:
/// the clock handed to [`Connection::with_clock`] and the one the caller keeps
/// are the same clock, so a record's stamp and a deadline computed beside it
/// cannot disagree.
///
/// Time can be set backwards, and an id minted afterwards is refused with
/// [`IdError::WentBackwards`] rather than absorbed, because an id that went
/// backwards would put a later revision earlier in every id-ordered scan.
///
/// [`Connection::with_clock`]: crate::Connection::with_clock
/// [`IdError::WentBackwards`]: crate::id::IdError::WentBackwards
#[derive(Debug, Clone, Default)]
pub struct HeldClock(Arc<AtomicU64>);

impl HeldClock {
    /// A clock reading this instant.
    pub fn at(Timestamp(millis): Timestamp) -> Self {
        Self(Arc::new(AtomicU64::new(millis)))
    }

    /// Moves it to this instant, for every holder at once.
    pub fn set(&self, Timestamp(millis): Timestamp) {
        self.0.store(millis, Ordering::SeqCst);
    }

    /// Moves it forward, saturating rather than wrapping at the end of the
    /// range, since a clock that wrapped would read as one that went
    /// backwards.
    pub fn advance(&self, millis: u64) {
        let mut held = self.0.load(Ordering::SeqCst);
        loop {
            let moved = held.saturating_add(millis);
            match self
                .0
                .compare_exchange(held, moved, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return,
                Err(current) => held = current,
            }
        }
    }
}

impl Clock for HeldClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.0.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clone shares the instant rather than copying it, which is what lets a
    /// test move the clock a connection is already holding.
    #[test]
    fn a_clone_reads_the_move() {
        let clock = HeldClock::at(Timestamp(1_000));
        let held = clock.clone();

        clock.advance(500);

        assert_eq!(held.now(), Timestamp(1_500));
        assert_eq!(clock.now(), held.now());
    }

    #[test]
    fn it_can_be_set_either_way() {
        let clock = HeldClock::at(Timestamp(1_000));

        clock.set(Timestamp(9));
        assert_eq!(clock.now(), Timestamp(9));

        clock.set(Timestamp(10_000));
        assert_eq!(clock.now(), Timestamp(10_000));
    }

    /// Wrapping would read as a clock that went backwards, which is the one
    /// thing an id-ordered scan cannot survive.
    #[test]
    fn it_saturates_rather_than_wrapping() {
        let clock = HeldClock::at(Timestamp(u64::MAX - 1));

        clock.advance(10);

        assert_eq!(clock.now(), Timestamp(u64::MAX));
    }
}
