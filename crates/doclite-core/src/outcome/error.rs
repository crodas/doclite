//! What can go wrong, named rather than described.

use std::fmt::Display;

use crate::model::index::{ColumnType, IndexId};
use crate::model::object::ObjectId;

/// An object's bytes and its type failing to agree.
///
/// The message is a `String` for the same reason
/// [`StorageError::Engine`]'s is: the condition belongs to a format this crate
/// does not name, and under a feature flag it cannot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    /// A value could not be turned into bytes.
    #[error("encoding an object: {0}")]
    Encoding(String),

    /// Bytes could not be turned back into the type asking for them.
    #[error("decoding an object: {0}")]
    Decoding(String),
}

impl CodecError {
    /// Takes anything printable, so a codec need not name its own error type
    /// here.
    pub fn encoding(cause: impl Display) -> Self {
        Self::Encoding(cause.to_string())
    }

    /// Takes anything printable, for the same reason
    /// [`CodecError::encoding`] does.
    pub fn decoding(cause: impl Display) -> Self {
        Self::Decoding(cause.to_string())
    }
}

/// An operation that could never run, caught before it is handed to an engine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    /// A step names an index the catalog was never told about.
    #[error("no index named {0}")]
    UnknownIndex(IndexId),

    /// A key of the wrong width, which the typed surface makes a compile error
    /// and the byte-level one catches here.
    #[error("index {index} takes {expected} columns, given {found}")]
    Arity {
        /// The index the key was written for.
        index: IndexId,
        /// How many columns that index declares.
        expected: usize,
        /// How many the key carries.
        found: usize,
    },

    /// A slot with no row to fill it, since only a template is instantiated per
    /// row.
    #[error("a row slot appears outside a for_each template")]
    RowOutsideTemplate,

    /// Nesting would make the work a caller asked for quadratic with nothing
    /// naming the second scan.
    #[error("a for_each template contains another for_each")]
    NestedForEach,

    /// The one step that can do unbounded work, so its scan has to say how far
    /// it goes.
    #[error("a for_each scan carries no limit")]
    UnboundedForEach,

    /// An id with its top bit set, which [`ObjectId`](crate::ObjectId) reserves
    /// so that a backend with signed integers can store one as an integer.
    #[error("object id {0} sets the reserved top bit; an id is sixty-three bits")]
    ObjectIdTooLarge(ObjectId),

    /// A value in a step could not be encoded, which is the caller's to hear
    /// about before an engine is involved.
    #[error(transparent)]
    Encode(#[from] CodecError),
}

/// The storage layer failing to give an answer at all.
///
/// A refused precondition is not in here. That is
/// [`Outcome::Aborted`](crate::Outcome::Aborted), which is an answer: the
/// operation did its job.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
    /// An engine was asked about an index it was never told about.
    #[error("no index named {0}")]
    UnknownIndex(IndexId),

    /// A row reached an engine at a width its index does not have.
    #[error("index {index} takes {expected} columns, given {found}")]
    Arity {
        /// The index the row was filed against.
        index: IndexId,
        /// How many columns that index declares.
        expected: usize,
        /// How many the row carries.
        found: usize,
    },

    /// One index declared two ways in one catalog.
    ///
    /// Caught where the catalog is declared rather than when a row is filed
    /// against it, because by then the disagreement looks like an arity nobody
    /// wrote.
    #[error("index {index} was declared as {first:?} and again as {second:?}")]
    Redeclared {
        /// The name both declarations used.
        index: IndexId,
        /// The declaration that got there first, which is the one in force.
        first: &'static [ColumnType],
        /// The declaration that disagreed with it.
        second: &'static [ColumnType],
    },

    /// Objects are only ever inserted, so an id that is taken is a bug rather
    /// than an overwrite.
    #[error("object {0} already exists")]
    DuplicateObject(ObjectId),

    /// An operation that could never run, caught before an engine saw it.
    #[error(transparent)]
    Invalid(#[from] BuildError),

    /// Losing a race costs a second walk, so this is contention that did not
    /// clear rather than a refusal.
    #[error("gave up after {attempts} attempts, each losing to a concurrent writer")]
    Conflict {
        /// How many walks were spent before giving up.
        attempts: usize,
    },

    /// Answers are positional, so a short list leaves every later answer
    /// against the wrong read.
    #[error("an engine answered {answered} of {asked} reads")]
    MissingAnswers {
        /// How many reads were handed over.
        asked: usize,
        /// How many came back.
        answered: usize,
    },

    /// An object's bytes did not decode into the type the query promised.
    #[error(transparent)]
    Decode(CodecError),

    /// A query's answer did not match the shape its type promised.
    ///
    /// A `Query<T>` builds its own plan, so the two sides cannot disagree.
    /// Naming the case is cheaper than proving it.
    #[error("a query answered in a shape its type does not describe")]
    ReadMismatch,

    /// An in-process engine's lock, which no later call can trust.
    #[error("a lock was poisoned by a panic while it was held")]
    Poisoned,

    /// A failure only the engine can describe.
    ///
    /// The one place a `String` is carried, because the condition belongs to a
    /// backend this crate cannot name.
    #[error("engine: {0}")]
    Engine(String),
}
