//! What the document layer refuses for.

use doclite_core::{CodecError, IndexId, StorageError};

use crate::id::IdError;

/// Why a document operation did not happen.
///
/// `doclite-core` keeps a refused precondition as `Outcome::Aborted`, because
/// at that level the operation did its job. Here the caller asked for the
/// insert, so a taken email is an error rather than an answer, and
/// `conn.insert(user).await?` reads the way it should.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
/// `#[non_exhaustive]` because the set of things that can go wrong is open:
/// ADR 0018.
#[non_exhaustive]
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

    /// A read named an index whose rows were still being filled, and whatever
    /// was filling it went away before it finished.
    ///
    /// The rebuild ran on a [`Spawner`](crate::Spawner) the caller supplied, so
    /// this is what a spawner that dropped the work looks like from the reading
    /// end. It is an error rather than a wait on purpose: a read parked on an
    /// index nothing will ever fill would otherwise hang with nothing to report.
    #[error("index {index} was still being rebuilt, and the rebuild went away")]
    Rebuilding {
        /// The index that is still missing rows.
        index: IndexId,
    },

    /// The store has indexes whose rows are missing and this handle was given
    /// nothing that could fill them.
    ///
    /// [`Connection::over`](crate::Connection::over) takes schemas, which say
    /// which indexes exist and nothing about how to derive a row for one.
    /// Refusing is the honest answer: parking every read forever would be
    /// worse, and answering out of an index with rows missing would be wrong.
    #[error("index {index} needs rebuilding, so open with `records!` rather than schemas")]
    NoRebuilder {
        /// The index that needs filling.
        index: IndexId,
    },

    /// The index every record is enumerated through is itself missing rows,
    /// while indexes derived from it are not.
    ///
    /// A rebuild reads `<record>_latest` to find every live revision, so that
    /// index is the one thing it cannot rebuild. A record type that is new to
    /// the store has *every* index building and needs no rebuild at all, which
    /// is ordinary; this one has lost only its enumeration source, which
    /// nothing in this crate can produce and which is refused rather than
    /// guessed at.
    #[error("index {index} is what the rest are derived from and is itself missing rows")]
    MissingLatest {
        /// The generated index that cannot be rebuilt.
        index: IndexId,
    },

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
