//! Every rule the suite runs, under one name each.
//!
//! The three modules are three kinds of rule, not three kinds of caller: they
//! all take a [`Fresh`](crate::Fresh) and answer a [`Failure`](crate::Failure).
//! Gathering them here is what lets `conformance!` be one list over one arm,
//! and two rules that collided on a name would be a compile error here rather
//! than a test that quietly replaced another.

pub use crate::cases::*;
pub use crate::programs::*;
pub use crate::properties::*;
