//! A scan and a template, handed over unexpanded.
//!
//! The executor would otherwise scan, instantiate the template once per row,
//! and hand an engine the rows that came out. A backend with a query language
//! can write the whole thing in one statement, and this is what it needs to.
//!
//! It is offered only when the `for_each` is the entire operation. Expanding a
//! template stages into the overlay a later precondition folds, so deferring
//! one with a step after it would change what that step observes rather than
//! only how the work was done. `docs/plan-pushdown.md` is the reasoning, and
//! [`Batch`](crate::Batch) is where it stopped being reasoning: a sweep is a
//! variant there, so a batch cannot carry one beside anything else.

use crate::engine::key_range::KeyRange;
use crate::model::index::IndexId;
use crate::operation::step::Step;

/// One pass over a range, writing what each row derives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sweep {
    /// Which index the pass scans.
    pub index: IndexId,
    /// The rows it covers, already resolved to bytes.
    pub range: KeyRange,
    /// Always present: `check` refuses a `for_each` whose scan is unbounded,
    /// because it is the one step that can do unbounded work.
    pub limit: Option<usize>,
    /// Instantiated once per row, in index order. A column may name the row it
    /// is being instantiated for, which is what makes it a template.
    pub template: Vec<Step>,
}
