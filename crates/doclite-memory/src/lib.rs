//! The in-process engine.
//!
//! Two maps behind one lock: objects by id, and one ordered set of rows per
//! index. It honours every hint it is given, which makes it the fastest engine
//! to be wrong against; the conformance suite exists to catch that by blanking
//! the hints and demanding the same answers.
//!
//! What a batch *means* is not here. This is a [`Store`], so `doclite-store`
//! supplies the assertions, the refusals and the write order, and what is left
//! is where the rows are kept. `MemoryEngine` is that store wearing the engine
//! seam.
//!
//! ```
//! use doclite_core::{exact, key, AbortCode, Db, ObjectId, Index, Operation, Outcome};
//! use doclite_memory::MemoryEngine;
//!
//! static USERS_BY_EMAIL: Index<1> = Index::new("users_by_email");
//! const DUPLICATE_EMAIL: AbortCode = AbortCode::new(100);
//!
//! # async fn example() -> Result<(), doclite_core::StorageError> {
//! let db = Db::open(MemoryEngine::new(), &[USERS_BY_EMAIL.schema()]).await?;
//!
//! let id = ObjectId(7);
//! let op = Operation::new()
//!     .require_absent(&USERS_BY_EMAIL, exact(key!["a@b.c"]), DUPLICATE_EMAIL)
//!     .put_object(id, b"a blob".to_vec())
//!     .put_index(&USERS_BY_EMAIL, key!["a@b.c"], id)
//!     .returning(id);
//!
//! assert_eq!(db.run(&op).await?, Outcome::Committed(id));
//! assert_eq!(db.run(&op).await?, Outcome::Aborted(DUPLICATE_EMAIL));
//! # Ok(())
//! # }
//! # tokio::runtime::Builder::new_current_thread()
//! #     .build()
//! #     .expect("a current-thread runtime")
//! #     .block_on(example())
//! #     .expect("the example runs");
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{RwLock, RwLockWriteGuard};

use doclite_core::{Answers, IndexId, IndexSchema, KeyRange, ObjectId, Reads, Row, StorageError};
use doclite_store::{Region, Store, StoreEngine};

/// Objects and indexes in memory, for a process that wants no file.
pub type MemoryEngine = StoreEngine<MemoryStore>;

#[derive(Default)]
struct State {
    schemas: BTreeMap<IndexId, usize>,
    objects: BTreeMap<ObjectId, Vec<u8>>,
    indexes: BTreeMap<IndexId, BTreeSet<Row>>,
}

/// Where the rows are, and nothing about what a batch means.
#[derive(Default)]
pub struct MemoryStore {
    state: RwLock<State>,
}

impl MemoryStore {
    /// An empty store, which is the only way one starts: nothing here survives
    /// the process.
    pub fn new() -> Self {
        Self::default()
    }
}

impl State {
    fn arity(&self, index: IndexId) -> Result<usize, StorageError> {
        self.schemas
            .get(&index)
            .copied()
            .ok_or(StorageError::UnknownIndex(index))
    }

    /// Rows come out of a `BTreeSet<Row>`, so index order costs nothing: it is
    /// `Row`'s own `Ord`, which is columns and then object id.
    fn rows(&self, index: IndexId, range: &KeyRange) -> Vec<Row> {
        self.indexes
            .get(&index)
            .into_iter()
            .flatten()
            .filter(|row| range.contains(row.key.components()))
            .cloned()
            .collect()
    }
}

impl Store for MemoryStore {
    type Region<'a> = Held<'a>;

    fn define(&self, indexes: &[IndexSchema]) -> Result<(), StorageError> {
        let mut state = self.state.write().map_err(|_| StorageError::Poisoned)?;
        for schema in indexes {
            state.schemas.insert(schema.id, schema.arity());
            state.indexes.entry(schema.id).or_default();
        }
        Ok(())
    }

    fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        let state = self.state.read().map_err(|_| StorageError::Poisoned)?;
        let mut answers = Answers {
            scans: Vec::with_capacity(reads.scans.len()),
            objects: Vec::with_capacity(reads.objects.len()),
        };

        for scan in reads.scans {
            state.arity(scan.index)?;
            let mut rows = state.rows(scan.index, scan.range);
            if let Some(limit) = scan.limit {
                rows.truncate(limit);
            }
            answers.scans.push(rows);
        }

        for id in reads.objects {
            answers.objects.push(state.objects.get(id).cloned());
        }

        Ok(answers)
    }

    fn exclusive(&self) -> Result<Held<'_>, StorageError> {
        Ok(Held(
            self.state.write().map_err(|_| StorageError::Poisoned)?,
        ))
    }
}

/// The whole store, held for as long as one batch takes.
pub struct Held<'a>(RwLockWriteGuard<'a, State>);

impl Region for Held<'_> {
    fn arity(&self, index: IndexId) -> Result<usize, StorageError> {
        self.0.arity(index)
    }

    fn rows(&self, index: IndexId, range: &KeyRange) -> Result<Vec<Row>, StorageError> {
        self.0.arity(index)?;
        Ok(self.0.rows(index, range))
    }

    fn has_object(&self, id: ObjectId) -> Result<bool, StorageError> {
        Ok(self.0.objects.contains_key(&id))
    }

    fn delete(&mut self, index: IndexId, range: &KeyRange) {
        if let Some(rows) = self.0.indexes.get_mut(&index) {
            rows.retain(|row| !range.contains(row.key.components()));
        }
    }

    fn put_object(&mut self, id: ObjectId, bytes: Vec<u8>) {
        self.0.objects.insert(id, bytes);
    }

    fn put_row(&mut self, index: IndexId, row: Row) {
        self.0.indexes.entry(index).or_default().insert(row);
    }
}
