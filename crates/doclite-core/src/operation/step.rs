//! The ordered list an operation reduces to.

use crate::model::column::Column;
use crate::model::index::IndexId;
use crate::model::object::{ObjectId, ObjectRef};
use crate::model::range::RangeSpec;
use crate::outcome::abort::AbortCode;
use crate::query::plan::ReadPlan;

/// What a precondition expects to find.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Expect {
    /// The range holds nothing.
    Absent,
    /// The range holds exactly this many rows.
    Count(u64),
}

/// One step of an operation, knowing nothing about builders or engines.
///
/// Preconditions and mutations are one list, not two, so a precondition placed
/// after a mutation observes it. That is the whole of read-your-own-writes, and
/// the reason no typestate separates the two halves.
///
/// The separation from the builder is what keeps a verifiable, encodable
/// representation available if an untrusted caller ever has to submit one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Step {
    /// A condition on a range of one index, aborting with `code` if it does not
    /// hold.
    Require {
        /// Which index to look in.
        index: IndexId,
        /// The rows the condition is about.
        range: RangeSpec,
        /// What has to be true of them.
        expect: Expect,
        /// What the operation answers with if it is not.
        code: AbortCode,
    },
    /// A condition that one object exists, aborting with `code` if it does not.
    RequireObject {
        /// Which object has to be there.
        id: ObjectId,
        /// What the operation answers with if it is not.
        code: AbortCode,
    },
    /// The object store is additive, so this is never an update.
    PutObject {
        /// The id the caller minted, which an engine refuses if it is taken.
        id: ObjectId,
        /// The body, which no layer below the caller interprets.
        bytes: Vec<u8>,
    },
    /// One row filed into one index.
    PutIndex {
        /// Which index the row goes in.
        index: IndexId,
        /// The key, whose components may still be template slots.
        columns: Vec<Column>,
        /// The object the row points at.
        object: ObjectRef,
    },
    /// Every row of a range retired, so a caller never enumerates what it is
    /// removing.
    DeleteIndex {
        /// Which index to retire rows from.
        index: IndexId,
        /// Everything in here goes.
        range: RangeSpec,
    },
    /// The template is instantiated once per scanned row. The only step that
    /// can do unbounded work, which is why its scan must carry a limit.
    ForEach {
        /// The rows to instantiate over, in index order.
        scan: Box<ReadPlan>,
        /// Run once per row, and the only place a row slot may appear.
        template: Vec<Step>,
    },
}

impl Step {
    /// Whether running this step changes anything.
    ///
    /// An operation with no mutating step never reaches an engine's `apply`,
    /// which is where the read-only lock mode went.
    pub fn mutates(&self) -> bool {
        match self {
            Self::Require { .. } | Self::RequireObject { .. } => false,
            Self::PutObject { .. } | Self::PutIndex { .. } | Self::DeleteIndex { .. } => true,
            Self::ForEach { template, .. } => template.iter().any(Self::mutates),
        }
    }
}
