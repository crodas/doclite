//! What an engine is asked for, and what it is allowed to ignore.
//!
//! An engine holds two things and decides nothing about either. Objects are
//! bytes under a [`ObjectId`](crate::ObjectId) the caller minted, and are
//! only ever inserted. Index rows are a column tuple and the object they
//! name, and those are inserted and deleted freely.
//!
//! # Hints, and the three things that are not
//!
//! Everything an [`IndexRead`] carries beyond the index itself is a hint. The
//! test is what happens when an engine ignores one: it returns a superset,
//! which the executor then filters, so ignoring costs time and never
//! correctness. The `doclite-testkit` conformance suite proves this by blanking
//! every hint and demanding the same answers.
//!
//! | Told | Cost of ignoring it |
//! |---|---|
//! | `range` | a superset; the executor filters |
//! | `limit` | every matching row; the executor trims |
//! | row order | **the wrong answer** |
//! | `apply` atomicity | **a corrupt store** |
//! | `apply` step order | **a corrupt store** |
//!
//! # Why there is no transaction handle
//!
//! An operation reads, then commits, and the two are separate calls. What
//! carries consistency across the gap is not a held lock but the
//! [`Assertion`]s the executor recorded while reading: `apply` re-evaluates
//! every one against pre-batch state and answers [`Applied::Conflict`] if any
//! has moved. The executor then re-runs the operation from the start, which is
//! safe because an operation is a value with no side effects.
//!
//! That also settles the lock mode without a flag. A read-only operation never
//! reaches `apply`, so an engine takes its write lock in exactly one method.

mod assertion;
mod batch;
mod defined;
mod key_range;
mod plan;
mod read;
mod sweep;

use async_trait::async_trait;

pub use assertion::{Assertion, Observed};
pub use batch::{Applied, Batch, IndexEntry, RangeDelete};
pub use defined::Defined;
pub use key_range::KeyRange;
pub use plan::Plan;
pub use read::{Answers, IndexRead, Reads};
pub use sweep::Sweep;

use crate::model::index::{IndexId, IndexSchema};
use crate::outcome::error::StorageError;
use crate::query::value::ReadValue;

/// Where objects live and what indexes name them.
#[async_trait]
pub trait Engine: Send + Sync {
    /// Declares one home per index, and says which of them cannot be trusted
    /// yet.
    ///
    /// No index carries a constraint. Whether at most one object may hold a
    /// key is a rule, and rules are asserted rather than declared.
    ///
    /// Called with the whole catalog on every open, so an engine that keeps its
    /// indexes is the one place a declaration can be compared against what was
    /// stored for it last time. An index this engine has no rows for, or rows
    /// in a shape the declaration no longer matches, is named in the
    /// [`Defined`] that comes back, and the layer above fills it before letting
    /// a read through. An engine that keeps nothing answers
    /// [`Defined::ready`].
    async fn define(&self, indexes: &[IndexSchema]) -> Result<Defined, StorageError>;

    /// Records that an index named by [`Engine::define`] now holds every row
    /// it should, so a later open does not fill it again.
    ///
    /// Defaulted to doing nothing, which is correct rather than lenient: an
    /// engine that never reported an index as building is never told one
    /// finished.
    async fn rebuilt(&self, index: IndexId) -> Result<(), StorageError> {
        let _ = index;
        Ok(())
    }

    /// Answers every scan and every object from one consistent view.
    ///
    /// One view across the whole call is what `Query::zip` is built on, and the
    /// only reason several reads can be trusted to agree. Both halves of
    /// [`Answers`] are positional: one entry per scan and one per object id,
    /// in the order asked.
    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError>;

    /// Applies a whole [`Batch`], or nothing.
    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError>;

    /// What this plan answers, if this engine planned it.
    ///
    /// `None` means it did not, and the read happens the ordinary way through
    /// [`Engine::read`]. That default is what makes the plan a hint in the
    /// sense the table above means: an engine that never implements this is
    /// unaffected, and one that does can only save work.
    ///
    /// A backend with a planner of its own is what this is for. A count
    /// answered here counts where the rows are instead of moving them, and the
    /// halves of a [`Plan::Zip`] can be one statement rather than two reads
    /// that have to be trusted to agree.
    ///
    /// An engine that can plan part of a request and not the rest declines the
    /// whole of it. There is no partial answer.
    async fn plan(&self, plan: &Plan) -> Result<Option<ReadValue>, StorageError> {
        let _ = plan;
        Ok(None)
    }

    /// Whether this engine takes a sweep unexpanded.
    ///
    /// Asked once with the sweep in hand, so an engine declines the templates
    /// it cannot write rather than promising in the abstract. Declining is the
    /// default and costs nothing: the executor expands the template itself, as
    /// it always has.
    ///
    /// Unlike [`Engine::plan`] this is not offered whenever one could be. A
    /// sweep reaches an engine only when the `for_each` is the whole operation,
    /// because expanding a template stages into the overlay a later
    /// precondition reads. Saying yes here is what makes the next `apply` a
    /// [`Batch::Sweep`], which carries nothing else because it cannot.
    async fn takes_sweep(&self, sweep: &Sweep) -> bool {
        let _ = sweep;
        false
    }
}
