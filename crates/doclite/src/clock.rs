//! Where a document's timestamp comes from.

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
