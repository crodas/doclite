//! What makes an engine correct, as a suite any engine can be run against.
//!
//! One macro invocation in an engine crate expands to one test per rule:
//!
//! ```text
//! async fn fresh() -> Result<MyEngine, StorageError> { .. }
//!
//! doclite_testkit::conformance!(
//!     #[tokio::test]
//!     fresh
//! );
//! ```
//!
//! The test harness is the walk. Every rule is its own test, so a failing
//! engine reports every rule it breaks in one run rather than the first, and
//! `cargo test a_stale_assertion` runs a single one during bring-up.
//!
//! # Two halves, and they are not the same question
//!
//! [`cases`] talks to an [`Engine`](doclite_core::Engine) directly and checks
//! the things that are not hints: rows come back in index order, a batch
//! applies whole or not at all, assertions see pre-batch state, and losing a
//! race is a conflict rather than an error.
//!
//! They are three modules and one list. Every rule builds its own engine from
//! a [`Fresh`], so which module a rule lives in is about what it checks rather
//! than about how it is called, and [`rules`] gathers them under one name each.
//!
//! [`properties`] runs the same script through the executor twice, once against
//! the engine and once against a [`Probe`] that strips every hint before
//! forwarding. If the two transcripts differ, the executor was leaning on the
//! engine for correctness rather than for speed.
//!
//! # Which runtime
//!
//! None of this crate's business, which is why the test attribute is the
//! caller's to pass. See ADR 0006.

mod catalog;
mod failure;
mod fresh;
mod probe;

pub mod cases;
pub mod programs;
pub mod properties;
pub mod rules;

pub use failure::Failure;
pub use fresh::Fresh;
pub use probe::Probe;

/// Expands to one test per rule an engine owes.
///
/// Takes the async test attribute to put on each generated test, then a type
/// implementing [`Fresh`]. It says how to build an engine, and a new one is
/// built per test, so no rule ever sees a store another rule wrote to.
///
/// ```text
/// struct Mine;
///
/// #[async_trait]
/// impl Fresh for Mine {
///     type Engine = MyEngine;
///     async fn fresh() -> Result<MyEngine, StorageError> { .. }
/// }
///
/// doclite_testkit::conformance!(
///     #[tokio::test]
///     Mine
/// );
/// ```
#[macro_export]
macro_rules! conformance {
    (#[$attr:meta] $engine:ty $(,)?) => {
        $crate::__conformance! {
            attr: #[$attr],
            engine_type: $engine,
            rules: [
                a_scan_answers_in_index_order,
                a_batch_applies_whole,
                a_refused_batch_changes_nothing,
                assertions_see_pre_batch_state,
                a_stale_assertion_is_a_conflict,
                reads_share_one_view,
                objects_answer_positionally,
                objects_are_additive,
                an_unknown_index_is_named,
                a_limited_assertion_judges_what_was_read,
                scans_answer_positionally,
                hints_are_hints,
                insert_a_user_if_the_email_is_unique,
                replace_a_logical_object,
                scan_a_range,
                a_dangling_entry_aborts_a_strict_scan,
                delete_every_entry_under_a_prefix,
                uniqueness_across_two_indexes,
                a_precondition_after_a_mutation_sees_it,
                two_operations_compose_into_one_transaction,
                a_missing_object_refuses_the_whole_operation,
                zip_answers_two_reads_from_one_view,
                for_each_writes_a_derived_entry_per_row,
                an_unbounded_for_each_is_refused_before_it_runs,
                for_each_deletes_a_range_named_by_each_row,
                a_for_each_under_its_limit_still_commits,
                a_for_each_whose_scan_names_no_row_writes_nothing,
                a_catalog_that_names_an_index_twice_is_refused,
                a_fresh_engine_owes_no_rows,
                an_index_reported_rebuilt_is_accepted,
            ],
        }
    };
}

/// One rule, expanded into one test.
///
/// Every rule has the same signature, whichever module it lives in, so this is
/// one arm over one list rather than three that could drift apart.
/// `tests/registry_drift.rs` is what says the list is the whole of them.
#[doc(hidden)]
#[macro_export]
macro_rules! __conformance {
    (
        attr: #[$attr:meta],
        engine_type: $engine:ty,
        rules: [$($rule:ident),* $(,)?],
    ) => {
        $(
            #[$attr]
            async fn $rule() -> ::core::result::Result<(), $crate::Failure> {
                $crate::rules::$rule::<$engine>().await
            }
        )*
    };
}
