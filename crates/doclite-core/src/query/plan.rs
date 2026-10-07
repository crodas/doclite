//! A read as plain data.

use crate::model::index::IndexId;
use crate::model::object::ObjectId;
use crate::model::range::RangeSpec;

/// Whether a scan also wants the objects its rows name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ObjectMode {
    /// Rows only.
    None,
    /// An index row naming a missing object aborts.
    Strict,
    /// An index row naming a missing object yields `None`.
    Lenient,
}

/// What to read, with no builder types and no engine types in it.
///
/// [`Query`](crate::Query) is the typed surface that produces one and the only
/// place the answer's shape is promised. Keeping this half plain is what would
/// let an operation cross a process boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ReadPlan {
    /// How many rows the range holds, and nothing about what they are.
    Count {
        /// Which index to count in.
        index: IndexId,
        /// The rows to count.
        range: RangeSpec,
    },
    /// One object's body, if it is there.
    Object {
        /// Which object.
        id: ObjectId,
    },
    /// The rows of a range, in index order.
    Scan {
        /// Which index to scan.
        index: IndexId,
        /// The rows to return, whose components may still be template slots.
        range: RangeSpec,
        /// At most this many, and what a `for_each` scan must carry.
        limit: Option<usize>,
        /// Whether the objects those rows name are wanted alongside them.
        objects: ObjectMode,
    },
    /// Two reads answered from one view of the store, which is the only reason
    /// they can be trusted to agree.
    Zip(Box<ReadPlan>, Box<ReadPlan>),
}

impl ReadPlan {
    /// Every index this read looks in.
    ///
    /// What it is for is an index whose rows are not all there yet: a read
    /// through one would answer out of a table with rows missing, which is
    /// wrong rather than slow, so a caller that knows an index is still being
    /// filled waits on exactly the reads that name it.
    pub fn reads(&self) -> Vec<IndexId> {
        let mut named = Vec::new();
        self.gather(&mut named);
        named
    }

    fn gather(&self, into: &mut Vec<IndexId>) {
        match self {
            Self::Object { .. } => {}
            Self::Count { index, .. } | Self::Scan { index, .. } => named_once(*index, into),
            Self::Zip(left, right) => {
                left.gather(into);
                right.gather(into);
            }
        }
    }
}

/// Named once, because a read that looks in one index twice waits for it once.
pub(crate) fn named_once(index: IndexId, into: &mut Vec<IndexId>) {
    if !into.contains(&index) {
        into.push(index);
    }
}
