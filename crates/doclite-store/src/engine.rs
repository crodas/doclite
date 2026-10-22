//! The adapter: a store, wearing the engine seam.

use async_trait::async_trait;
use doclite_core::{
    Answers, Applied, Batch, Defined, Engine, IndexId, IndexSchema, Reads, StorageError,
};

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
    async fn define(&self, indexes: &[IndexSchema]) -> Result<Defined, StorageError> {
        self.0.define(indexes)
    }

    /// Forwarded rather than left to the default, because the record has to
    /// outlive the process and a `StoreEngine` holds nothing but the store.
    async fn rebuilt(&self, index: IndexId) -> Result<(), StorageError> {
        self.0.rebuilt(index)
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use doclite_core::{KeyRange, ObjectId, Row};

    use super::*;
    use crate::store::Region;

    const ONE: IndexId = IndexId("one");

    /// A store that answers only the two questions this test is about.
    ///
    /// What a batch means is `admit.rs`'s to prove, so the region refuses every
    /// read rather than pretending to hold rows.
    #[derive(Default)]
    struct Recording {
        filled: AtomicBool,
    }

    impl Store for Recording {
        type Region<'a> = Nowhere;

        fn define(&self, _indexes: &[IndexSchema]) -> Result<Defined, StorageError> {
            match self.filled.load(Ordering::Relaxed) {
                true => Ok(Defined::ready()),
                false => Ok(Defined::building(vec![ONE])),
            }
        }

        fn rebuilt(&self, _index: IndexId) -> Result<(), StorageError> {
            self.filled.store(true, Ordering::Relaxed);
            Ok(())
        }

        fn read(&self, _reads: &Reads<'_>) -> Result<Answers, StorageError> {
            Ok(Answers {
                scans: Vec::new(),
                objects: Vec::new(),
            })
        }

        fn exclusive(&self) -> Result<Self::Region<'_>, StorageError> {
            Ok(Nowhere)
        }
    }

    struct Nowhere;

    impl Region for Nowhere {
        fn arity(&self, index: IndexId) -> Result<usize, StorageError> {
            Err(StorageError::UnknownIndex(index))
        }

        fn rows(&self, index: IndexId, _range: &KeyRange) -> Result<Vec<Row>, StorageError> {
            Err(StorageError::UnknownIndex(index))
        }

        fn has_object(&self, _id: ObjectId) -> Result<bool, StorageError> {
            Ok(false)
        }

        fn delete(&mut self, _index: IndexId, _range: &KeyRange) {}

        fn put_object(&mut self, _id: ObjectId, _bytes: Vec<u8>) {}

        fn put_row(&mut self, _index: IndexId, _row: Row) {}
    }

    /// A `StoreEngine` keeps nothing, so the store underneath has to be the
    /// thing told a fill happened. Leaving the forward out compiles, because
    /// `Engine::rebuilt` is defaulted, and the store then reports the same
    /// index on every open for ever.
    #[tokio::test]
    async fn a_fill_reaches_the_store_underneath() -> Result<(), StorageError> {
        let engine = StoreEngine::over(Recording::default());

        let building = engine.define(&[]).await?;
        assert_eq!(building.indexes(), [ONE]);

        engine.rebuilt(ONE).await?;
        assert!(engine.define(&[]).await?.is_ready());
        Ok(())
    }
}
