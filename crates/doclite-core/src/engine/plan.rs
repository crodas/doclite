//! What a read asked for, before it is taken apart into flat scans.
//!
//! [`Reads`](crate::Reads) is a flat list, and flattening loses two things a
//! backend with a planner could have used: that a count is a count rather than
//! every row, and that the halves of a zip belong together. This is the same
//! request with neither lost.
//!
//! It mirrors [`ReadPlan`](crate::ReadPlan) with every range already resolved,
//! because a range in a plan is still a spec that may name a template slot and
//! an engine has no business resolving one.

use crate::engine::key_range::KeyRange;
use crate::model::index::IndexId;
use crate::model::object::ObjectId;
use crate::query::plan::ObjectMode;

/// One read, in the shape the caller asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// How many rows the range holds, and nothing about what they are.
    Count {
        /// Which index to count in.
        index: IndexId,
        /// The rows to count.
        range: KeyRange,
    },
    /// One object's body, if it is there.
    Object {
        /// Which object.
        id: ObjectId,
    },
    /// The rows of a range, and their objects when they were asked for.
    Rows {
        /// Which index to scan.
        index: IndexId,
        /// The rows to return.
        range: KeyRange,
        /// At most this many, and a hint in one direction: more is allowed,
        /// fewer is not.
        limit: Option<usize>,
        /// Whether the objects those rows name are wanted alongside them.
        objects: ObjectMode,
    },
    /// Two reads that have to come from one view, which is the whole reason
    /// they are one request rather than two.
    Zip(Box<Plan>, Box<Plan>),
}
