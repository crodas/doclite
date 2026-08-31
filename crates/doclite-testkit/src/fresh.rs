//! How the suite gets an engine to run against.
//!
//! It is implemented on a type the engine crate names rather than on the engine
//! itself, which is what keeps it possible: an engine assembled from types
//! another crate owns, as `StoreEngine<MemoryStore>` is, cannot carry a trait
//! this crate defined.
//!
//! It is a trait rather than a constructor passed in because the suite is
//! generic over what makes an engine, not over a closure: a rule names the type
//! it wants and the type says how to build one.

use async_trait::async_trait;
use doclite_core::{Engine, StorageError};

/// Something that makes engines for the suite to run against.
#[async_trait]
pub trait Fresh {
    /// The engine this builds.
    type Engine: Engine;

    /// A new engine holding nothing.
    ///
    /// Called once per generated test, and twice within a single one, so two of
    /// them have to be able to exist at the same time without sharing a store.
    async fn fresh() -> Result<Self::Engine, StorageError>;
}
