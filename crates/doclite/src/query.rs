//! The half of a generated query builder that is the same for every record.
//!
//! A generated `by_` method knows its index and its arity, so it builds the
//! scan itself and hands it here. Everything after that point, the limit, the
//! terminals and the shape that comes back, does not depend on the record and
//! lives once.

use std::marker::PhantomData;

use doclite_core::{bounded, prefix, Bound, Index, Key, Query, Range, RangeSpec, Row};

use crate::clock::Clock;
use crate::connection::Connection;
use crate::error::DocumentError;
use crate::record::Record;
use crate::revision::Stored;
use doclite_core::Engine;

/// Rows to fetch when `get_one` is asked for: one to answer with, one to catch
/// a second match rather than silently picking a winner.
const ONE_PLUS_A_WITNESS: usize = 2;

/// A scan being assembled.
pub struct Selection<'a, E, C, T> {
    conn: &'a Connection<E, C>,
    scan: Option<Query<Vec<Row>>>,
    limit: Option<usize>,
    record: PhantomData<fn() -> T>,
}

impl<'a, E: Engine, C: Clock, T: Record> Selection<'a, E, C, T> {
    /// A selection with no index chosen yet, which a generated `by_` method
    /// then names.
    pub fn new(conn: &'a Connection<E, C>) -> Self {
        Self {
            conn,
            scan: None,
            limit: None,
            record: PhantomData,
        }
    }

    /// Called by a generated `by_` method with the scan it built.
    ///
    /// The last call wins, so naming two indexes is the second one, not an
    /// intersection. An index answers one question at a time.
    pub fn on(mut self, scan: Query<Vec<Row>>) -> Self {
        self.scan = Some(scan);
        self
    }

    /// A ceiling rather than a count: fewer matching rows is an answer, not a
    /// refusal.
    pub fn limit(mut self, rows: usize) -> Self {
        self.limit = Some(rows);
        self
    }

    /// Every document that matched, in the index's own order.
    pub async fn get_many(self) -> Result<Vec<Stored<T>>, DocumentError> {
        let scan = self.scan.ok_or(DocumentError::NoIndex)?;
        self.conn.select(scan, self.limit).await
    }

    /// The one document that matched.
    ///
    /// A second match is [`DocumentError::NotUnique`] rather than an arbitrary
    /// winner, because what comes back is editable: silently editing one of
    /// several is a lost write nobody asked about.
    pub async fn get_one(self) -> Result<Stored<T>, DocumentError> {
        let scan = self.scan.ok_or(DocumentError::NoIndex)?;
        let mut found = self.conn.select(scan, Some(ONE_PLUS_A_WITNESS)).await?;
        if found.len() > 1 {
            return Err(DocumentError::NotUnique);
        }
        found.pop().ok_or(DocumentError::NotFound)
    }
}

/// Every row whose key starts with these components.
///
/// The generated `by_` method assembles the key, because only it knows how many
/// components its index takes.
pub fn scan_prefix<const N: usize, const K: usize>(
    index: &Index<N>,
    key: Key<K>,
) -> Query<Vec<Row>> {
    Query::scan(index, prefix::<N, K>(key))
}

/// Every row between two keys, either of which may be absent.
pub fn scan_between<const N: usize, const K: usize>(
    index: &Index<N>,
    lo: Option<(Key<K>, Bound)>,
    hi: Option<(Key<K>, Bound)>,
) -> Query<Vec<Row>> {
    Query::scan(index, bounded::<N, K>(lo, hi))
}

/// Every row between two bounds over the last component of a prefix.
///
/// The components before the one being ranged are pinned, and an end the caller
/// left open falls back to that prefix rather than to the end of the index.
/// That fallback is the whole reason this exists: `by_status_due_at(Open,
/// ..=now)` written with an absent lower bound drops the status from the low
/// end and answers with every status that sorts below `Open`, which is a work
/// queue quietly returning closed work.
///
/// `pinned` is `None` only when the ranged component is the first one, where
/// there is nothing to fall back to and open really is the end of the index.
pub fn scan_within<const N: usize, const W: usize, const P: usize>(
    index: &Index<N>,
    lo: Option<(Key<W>, Bound)>,
    hi: Option<(Key<W>, Bound)>,
    pinned: Option<Key<P>>,
) -> Query<Vec<Row>> {
    let group = |bound: Option<(Key<W>, Bound)>| match (bound, pinned.clone()) {
        (Some((key, bound)), _) => Some((key.into_columns(), bound)),
        (None, Some(prefix)) => Some((prefix.into_columns(), Bound::Incl)),
        (None, None) => None,
    };

    let spec = RangeSpec::Between {
        lo: group(lo),
        hi: group(hi),
    };
    Query::scan(index, Range::<N, N>::from_spec(spec))
}
