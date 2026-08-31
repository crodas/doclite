//! What an engine is asked to read, and what it hands back.

use crate::engine::key_range::KeyRange;
use crate::model::index::IndexId;
use crate::model::object::ObjectId;
use crate::model::row::Row;

/// One read of one index.
#[derive(Debug, Clone, Copy)]
pub struct IndexRead<'a> {
    /// Which index to scan.
    pub index: IndexId,
    /// Which rows are wanted. A hint: the executor checks every row it gets
    /// back, so an engine may return more and must not return fewer.
    pub range: &'a KeyRange,
    /// Rows to return at most.
    ///
    /// A hint in one direction only: an engine may return every matching row,
    /// and must not return fewer than this many when more match.
    pub limit: Option<usize>,
}

impl<'a> IndexRead<'a> {
    /// Unlimited, because a limit is the exception and naming it at every call
    /// site would put it on every caller's mind.
    pub fn new(index: IndexId, range: &'a KeyRange) -> Self {
        Self {
            index,
            range,
            limit: None,
        }
    }
}

/// Everything one call reads, answered from one view.
#[derive(Debug, Clone, Copy)]
pub struct Reads<'a> {
    /// One row set comes back per scan, in this order.
    pub scans: &'a [IndexRead<'a>],
    /// One slot comes back per id, in this order.
    pub objects: &'a [ObjectId],
}

impl<'a> Reads<'a> {
    /// Scans and no objects, which is what a precondition asks for.
    pub fn scans(scans: &'a [IndexRead<'a>]) -> Self {
        Self {
            scans,
            objects: &[],
        }
    }

    /// Objects and no scans, which is what following a reference asks for.
    pub fn objects(objects: &'a [ObjectId]) -> Self {
        Self {
            scans: &[],
            objects,
        }
    }
}

/// What a [`Reads`] produced.
///
/// One row set per scan and one slot per object, each in the order it was
/// asked for. Answering positionally is what keeps "which answer is this" from
/// being a question the caller has to work out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answers {
    /// Rows in the index's own order: columns, then object id.
    pub scans: Vec<Vec<Row>>,
    /// One slot per requested id. An object that is not there is `None`.
    pub objects: Vec<Option<Vec<u8>>>,
}
