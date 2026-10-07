//! An engine with every hint taken away from it.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use doclite_core::{
    Answers, Applied, Batch, Defined, Engine, IndexId, IndexRead, IndexSchema, KeyRange, Plan,
    ReadValue, Reads, StorageError, Sweep,
};

/// Wraps an engine and blanks every hint before forwarding.
///
/// `define` and `rebuilt` are the two methods it forwards untouched. The hints
/// are about what an engine is *told*, and blanking one only ever widens what
/// comes back; what `define` answers is what the engine *holds*, and blanking
/// that would make the engine lie about an index whose rows are missing. See
/// ADR 0004.
///
/// The range becomes "everything" and the limit goes. An engine wrapped in this
/// must still produce the same answers through the executor, because filtering
/// and trimming are the executor's job and the hints only ever saved it work.
pub struct Probe<E> {
    inner: E,
    everything: KeyRange,
    reads: AtomicUsize,
}

impl<E> Probe<E> {
    /// Wraps an engine that is about to be lied to about every range and limit.
    pub fn new(inner: E) -> Self {
        Self {
            inner,
            everything: KeyRange::All,
            reads: AtomicUsize::new(0),
        }
    }

    /// How many reads reached the engine underneath.
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl<E: Engine> Engine for Probe<E> {
    /// Forwarded whole, because what `define` reports is not a hint: an engine
    /// that claims an index is complete when its rows are missing answers a
    /// query wrongly rather than slowly, so there is nothing here to strip.
    async fn define(&self, indexes: &[IndexSchema]) -> Result<Defined, StorageError> {
        self.inner.define(indexes).await
    }

    async fn rebuilt(&self, index: IndexId) -> Result<(), StorageError> {
        self.inner.rebuilt(index).await
    }

    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        self.reads
            .fetch_add(reads.scans.len() + reads.objects.len(), Ordering::Relaxed);
        let blanked: Vec<IndexRead<'_>> = reads
            .scans
            .iter()
            .map(|scan| IndexRead {
                index: scan.index,
                range: &self.everything,
                limit: None,
            })
            .collect();
        self.inner
            .read(&Reads {
                scans: &blanked,
                objects: reads.objects,
            })
            .await
    }

    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError> {
        self.inner.apply(batch).await
    }

    /// Declines every plan, whatever the engine underneath would have done
    /// with it.
    ///
    /// This is the same stripping the other hints get. An engine that answers a
    /// count in its own language and one that is handed every row and counts
    /// them have to agree, and running the script both ways is what says so.
    async fn plan(&self, plan: &Plan) -> Result<Option<ReadValue>, StorageError> {
        let _ = plan;
        Ok(None)
    }

    /// Declines every sweep, so the template is expanded row by row.
    ///
    /// The same stripping the other hints get. An engine that writes a whole
    /// `for_each` in its own language and one that is handed the rows have to
    /// leave the store in the same state, and running the script both ways is
    /// what says so.
    async fn takes_sweep(&self, sweep: &Sweep) -> bool {
        let _ = sweep;
        false
    }
}
