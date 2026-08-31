//! What an engine is asked to write, and the conditions it is refused under.
//!
//! There is no transaction handle, so what carries consistency from the reads
//! to the write is the [`Assertion`](super::Assertion) list a batch arrives
//! with rather than a held lock.

use crate::engine::assertion::Assertion;
use crate::engine::key_range::KeyRange;
use crate::engine::sweep::Sweep;
use crate::model::index::IndexId;
use crate::model::object::ObjectId;

/// One index row to file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Which index the row belongs to.
    pub index: IndexId,
    /// The columns, already encoded.
    pub columns: Vec<Vec<u8>>,
    /// The object the row points at.
    pub object: ObjectId,
}

/// A run of index rows to retire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeDelete {
    /// Which index to retire rows from.
    pub index: IndexId,
    /// Every row in here goes, so a caller never enumerates what it removes.
    pub range: KeyRange,
}

/// Everything one transaction does.
///
/// A sweep is a variant rather than a field, so "a batch carries a sweep only
/// when it carries nothing else" is a shape rather than a rule somebody has to
/// keep. It is also what stops an engine forgetting one: the match is
/// exhaustive, and a backend that cannot write a sweep says so where it would
/// otherwise have written nothing and reported success.
#[derive(Debug, Clone, Copy)]
pub enum Batch<'a> {
    /// The ordinary case. The order is the contract, not the field order: an
    /// engine evaluates every assertion against pre-batch state, then deletes
    /// index rows, then inserts objects, then inserts index rows.
    Writes {
        /// Every one is evaluated against pre-batch state, and one that no longer
        /// holds refuses the batch.
        asserts: &'a [Assertion],
        /// Insertions only. An engine refuses an id it already holds.
        objects: &'a [(ObjectId, Vec<u8>)],
        /// Retired before anything is inserted, so a key can be released and
        /// reclaimed in one batch.
        delete: &'a [RangeDelete],
        /// Filed last, once the deletes have freed whatever they were going to.
        insert: &'a [IndexEntry],
    },
    /// A scan whose template the engine said it would write itself.
    ///
    /// The whole operation, so there are no assertions to carry: a sweep is
    /// offered only for a lone `for_each`, and nothing ran before it to record
    /// one.
    Sweep(&'a Sweep),
}

/// Whether the batch landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The batch landed and is durable to whatever the engine promises.
    Committed,
    /// An assertion no longer held. Not an error: the executor re-runs.
    Conflict,
}
