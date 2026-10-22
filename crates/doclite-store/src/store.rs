//! What a store keeps, and the one region a batch is decided in.

use doclite_core::{
    Answers, Defined, IndexId, IndexSchema, KeyRange, ObjectId, Reads, Row, StorageError,
};

/// Somewhere ordered rows and objects live.
///
/// The narrow half of the two seams ADR 0011 describes. An
/// [`Engine`](doclite_core::Engine) is told about a whole batch and may admit it
/// however it likes; a `Store` is told about rows, and
/// [`StoreEngine`](crate::StoreEngine) supplies the rest. Implement this when
/// the substrate keeps ordered bytes and nothing more.
///
/// Every method is synchronous, because the substrates this is for are:
/// an in-process map, RocksDB, sled, redb. A store that waits on a network
/// should implement `Engine` directly, where ADR 0006 already put the async.
pub trait Store: Send + Sync {
    /// The exclusive region [`Store::exclusive`] hands out.
    type Region<'a>: Region
    where
        Self: 'a;

    /// Declares one home per index, and says which of them has rows missing.
    ///
    /// Called again with the same indexes on every open, so it has to tolerate
    /// repetition. A store that keeps nothing across a restart answers
    /// [`Defined::ready`], because a store built from nothing cannot be behind
    /// what was declared. A store that does keep its rows, which is every one
    /// ADR 0011 names as the reason this seam exists, is the only place a
    /// previous run's shape was written down, so it is the only place that can
    /// notice a declaration it has no complete set of rows for.
    fn define(&self, indexes: &[IndexSchema]) -> Result<Defined, StorageError>;

    /// Records that an index [`Store::define`] named now holds every row it
    /// should, so a later open does not fill it again.
    ///
    /// Required rather than defaulted, unlike
    /// [`Engine::rebuilt`](doclite_core::Engine::rebuilt): the stores this
    /// seam exists for are the ones that keep their rows, and one with nowhere
    /// to record a fill reports the same index building on every open for
    /// ever while answering every query correctly.
    fn rebuilt(&self, index: IndexId) -> Result<(), StorageError>;

    /// Answers every scan and every object from one consistent view.
    ///
    /// The range and the limit on a scan are hints: answering with more costs
    /// time, answering with fewer is wrong.
    fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError>;

    /// One region in which a batch is judged and then written, with no other
    /// writer in between.
    ///
    /// Holding this is what makes an assertion mean anything: it is evaluated
    /// against the state before the batch, and nothing may move between that
    /// and the write.
    fn exclusive(&self) -> Result<Self::Region<'_>, StorageError>;
}

/// A store held still, long enough to decide a batch and apply it.
///
/// Reads are fallible and writes are not, and that split is the whole of
/// atomicity here: everything that can refuse the batch is asked before
/// anything moves, so there is never a half-written region to unwind.
pub trait Region {
    /// How many columns a key of this index has. `Err` if the index is unknown.
    fn arity(&self, index: IndexId) -> Result<usize, StorageError>;

    /// The rows of this index in this range, in index order: columns, then
    /// object id.
    fn rows(&self, index: IndexId, range: &KeyRange) -> Result<Vec<Row>, StorageError>;

    /// Whether the object is already there, which is what refuses a duplicate
    /// before anything is written.
    fn has_object(&self, id: ObjectId) -> Result<bool, StorageError>;

    /// Retires every row of the range. Infallible, because by here the batch
    /// has already been judged.
    fn delete(&mut self, index: IndexId, range: &KeyRange);

    /// Files an object whose id was already found free.
    fn put_object(&mut self, id: ObjectId, bytes: Vec<u8>);

    /// Files a row into an index that was already found to exist.
    fn put_row(&mut self, index: IndexId, row: Row);
}
