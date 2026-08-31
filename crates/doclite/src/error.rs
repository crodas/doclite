//! What the document layer refuses for.

use doclite_core::{CodecError, StorageError};

use crate::id::IdError;

/// Why a document operation did not happen.
///
/// `doclite-core` keeps a refused precondition as `Outcome::Aborted`, because
/// at that level the operation did its job. Here the caller asked for the
/// insert, so a taken email is an error rather than an answer, and
/// `conn.insert(user).await?` reads the way it should.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    /// A `#[unique]` key is taken, and the message names which one so a caller
    /// with two of them can tell them apart.
    #[error("a document already holds this {index}")]
    Duplicate {
        /// The index that refused, named as it was declared.
        index: &'static str,
    },

    /// The revision being edited is no longer the head, so the edit was written
    /// against a version somebody has already replaced.
    #[error("the document was superseded while it was being edited")]
    Stale,

    /// A query whose filters match no declared index, which is a schema
    /// question rather than an empty result.
    #[error("a query named no index, so there is nothing to scan")]
    NoIndex,

    /// A read that promised one document found none.
    #[error("no document matched")]
    NotFound,

    /// A read that promised one document found several, which means the index
    /// it went through carries no uniqueness rule.
    #[error("more than one document matched where one was asked for")]
    NotUnique,

    /// A code this layer did not raise, which means an operation built by hand
    /// reached a `Connection` method.
    #[error("an operation aborted with an unrecognised code: {0}")]
    Aborted(doclite_core::AbortCode),

    /// The storage layer failed to give an answer at all.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// The id mint could not produce one.
    #[error(transparent)]
    Id(#[from] IdError),

    /// A record's bytes and its type failed to agree.
    #[error(transparent)]
    Codec(#[from] CodecError),
}
