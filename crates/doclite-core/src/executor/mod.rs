//! How a value becomes engine calls.
//!
//! Two machines, one per kind of work: [`walk`] runs an operation against the
//! overlay it is building, [`answer`] issues a query's reads in one call and
//! puts the answers back into the plan's shape. [`bind`] and [`delta`] are what
//! both of them lean on.

pub(crate) mod answer;
pub(crate) mod bind;
pub(crate) mod delta;
pub(crate) mod walk;
