//! The adapter: a store, wearing the engine seam.

use async_trait::async_trait;
use doclite_core::{Answers, Applied, Batch, Engine, IndexSchema, Reads, StorageError};

use crate::admit::Admission;
use crate::store::Store;

/// An [`Engine`] made out of a [`Store`].
///
/// The batch protocol lives behind this and nowhere else: assertions are
/// judged, an object that is already there is refused, an index that is not
/// there is named, and the write happens in the order a batch promises. A
/// store contributes where the rows are kept and nothing about what a batch
/// means.
///
/// Async stops here. [`Store`] is synchronous because the substrates it is for
/// are, and this is the layer ADR 0006 asked to be async.
pub struct StoreEngine<S>(S);

impl<S> StoreEngine<S> {
    /// Wraps a store that was built some particular way.
    pub const fn over(store: S) -> Self {
        Self(store)
    }

    /// The store underneath, for the store-specific things this seam hides.
    pub fn store(&self) -> &S {
        &self.0
    }
}

impl<S: Default> StoreEngine<S> {
    /// Wraps a store built its own default way, which is what an in-process
    /// engine wants.
    pub fn new() -> Self {
        Self(S::default())
    }
}

impl<S: Default> Default for StoreEngine<S> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<S: Store> Engine for StoreEngine<S> {
    async fn define(&self, indexes: &[IndexSchema]) -> Result<(), StorageError> {
        self.0.define(indexes)
    }

    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        self.0.read(reads)
    }

    /// A store never takes a sweep, and `Admission::over` is where that is
    /// enforced rather than assumed.
    ///
    /// It is the same reason a store cannot be a durable engine: a `Region`'s
    /// writes cannot fail and it has no commit point, so there is nowhere for a
    /// scan and the writes it derives to be one atomic act.
    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError> {
        let mut region = self.0.exclusive()?;
        let mut admission = Admission::over(&mut region, batch)?;
        if !admission.admits()? {
            return Ok(Applied::Conflict);
        }
        admission.commit();
        Ok(Applied::Committed)
    }
}
