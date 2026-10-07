//! What `define` answers: which of the indexes just declared cannot be
//! trusted yet.
//!
//! An engine that keeps nothing across a restart has nothing to distrust and
//! answers [`Defined::ready`]. One that does keep its indexes compares what it
//! was handed against what it stored, and names the ones whose rows do not
//! match. Naming them is safe where reading them back would not be: the ids
//! come from the declarations the caller just passed in, so each is already the
//! `&'static str` an [`IndexId`] needs.

use crate::model::index::IndexId;

/// The indexes a `define` could not vouch for.
///
/// This is not a hint in [`ADR 0004`](crate::engine)'s sense. Ignoring a hint
/// returns a superset the executor then filters; an engine that wrongly reports
/// an index ready answers a query out of an index that is missing rows, which
/// is a wrong answer and not a slow one.
///
/// `#[must_use]` because dropping it on the floor is the bug this type exists
/// to prevent.
#[must_use]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Defined {
    building: Vec<IndexId>,
}

impl Defined {
    /// Every index declared is complete.
    ///
    /// The truthful answer for an engine that holds nothing across a restart,
    /// because a store built from nothing cannot be behind.
    pub fn ready() -> Self {
        Self {
            building: Vec::new(),
        }
    }

    /// These indexes are declared but their rows are not filled.
    pub fn building(building: Vec<IndexId>) -> Self {
        Self { building }
    }

    /// Which indexes have rows missing, in the order the engine named them.
    pub fn indexes(&self) -> &[IndexId] {
        &self.building
    }

    /// Whether this one has rows missing.
    pub fn is_building(&self, index: IndexId) -> bool {
        self.building.contains(&index)
    }

    /// Whether anything is outstanding, which is the only question most
    /// callers have.
    pub fn is_ready(&self) -> bool {
        self.building.is_empty()
    }
}
