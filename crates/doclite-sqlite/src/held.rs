//! The database held still inside one transaction.
//!
//! Every statement this engine runs goes through one of these. It holds the
//! transaction and the catalog together because almost nothing needs one
//! without the other: a scan is a range against an index, and an index is only
//! a table once the catalog has said which.
//!
//! Dropping one rolls back, which is what every early return here relies on.
//! Committing consumes it, so a batch cannot be judged and then forgotten.
//!
//! # Judging and writing are two types
//!
//! A batch is judged before anything moves and written after, and the order is
//! the whole of atomicity here. It used to be one type on which either method
//! compiled, so `write` without `admits` was a sequencing mistake nothing
//! caught. [`Judging`] is consumed by [`Judging::admits`] and answers a
//! [`Writing`], so the order is what the types say rather than what a caller
//! remembers.

use std::collections::BTreeMap;

use doclite_core::{IndexEntry, IndexId, KeyRange, ObjectId, RangeDelete, Row, StorageError};
use rusqlite::{params_from_iter, Transaction, TransactionBehavior};

use crate::catalog::{Catalog, Layout, DOCUMENTS, JSON_DOCUMENTS};
use crate::connection::{failed, Open};
use crate::statement::Statement;
use crate::storage::{Body, ObjectKey};

/// How many ids one statement asks about at a time.
///
/// SQLite bounds how many parameters a statement may bind, and a scan of a
/// large index would otherwise write a longer one than it accepts.
const PER_STATEMENT: usize = 500;

pub(crate) struct Held<'a> {
    transaction: Transaction<'a>,
    catalog: &'a Catalog,
}

impl<'a> Held<'a> {
    /// One view of the store, for reads that have to agree with each other.
    pub(crate) fn reading(open: &'a mut Open) -> Result<Self, StorageError> {
        let (db, catalog) = open.parts();
        let transaction = db.transaction().map_err(failed)?;
        Ok(Self {
            transaction,
            catalog,
        })
    }

    /// The index, named rather than guessed at when it is not there.
    pub(crate) fn layout(&self, index: IndexId) -> Result<&Layout, StorageError> {
        self.catalog.get(index)
    }

    pub(crate) fn catalog(&self) -> &Catalog {
        self.catalog
    }

    /// The rows of this index in this range, in index order.
    ///
    /// Filtered exactly rather than widely. A superset is legal on a read and
    /// the executor would filter it, but this same answer judges an assertion
    /// inside `apply`, where a superset turns a satisfied assertion into a
    /// refused batch.
    pub(crate) fn rows(
        &self,
        index: IndexId,
        range: &KeyRange,
        limit: Option<usize>,
    ) -> Result<Vec<Row>, StorageError> {
        let layout = self.layout(index)?;
        let arity = layout.arity();
        let select = layout.select(layout.predicate(range)?, limit)?;

        let mut statement = self
            .transaction
            .prepare_cached(select.sql())
            .map_err(failed)?;
        let mut found = statement
            .query(params_from_iter(select.values()))
            .map_err(failed)?;

        let mut answer = Vec::new();
        while let Some(row) = found.next().map_err(failed)? {
            let mut key = Vec::with_capacity(arity);
            for at in 0..arity {
                let value = row.get_ref(at).map_err(failed)?;
                key.push(layout.columns()[at].decode(value)?);
            }
            let id: i64 = row.get(arity).map_err(failed)?;
            answer.push(Row::new(key, ObjectKey::read(id)?.id()));
        }
        Ok(answer)
    }

    /// Every row of the range, with no limit applied.
    ///
    /// What an assertion is judged on. `Observed::holds` applies the limit
    /// itself, taking the first rows of what it is given, so an engine that
    /// re-ran the range under its own limit would refuse batches that should
    /// land. `docs/sqlite.md` calls that the most expensive thing here to get
    /// wrong, so judging has this and no way to ask for fewer.
    pub(crate) fn every_row(
        &self,
        index: IndexId,
        range: &KeyRange,
    ) -> Result<Vec<Row>, StorageError> {
        self.rows(index, range, None)
    }

    /// How many rows the range holds, counted where they are.
    pub(crate) fn count(&self, index: IndexId, range: &KeyRange) -> Result<u64, StorageError> {
        let layout = self.layout(index)?;
        let count = layout.count(layout.predicate(range)?)?;
        let found = self
            .transaction
            .prepare_cached(count.sql())
            .map_err(failed)?
            .query_row(params_from_iter(count.values()), |row| row.get::<_, i64>(0))
            .map_err(failed)?;
        Ok(found as u64)
    }

    /// Both tables, because an id is one object wherever its bytes went and
    /// `apply` asks this before it files anything. A per-table primary key
    /// cannot see across the two, so this is what keeps an object from being
    /// written twice.
    pub(crate) fn has_object(&self, id: ObjectId) -> Result<bool, StorageError> {
        self.transaction
            .prepare_cached(&format!(
                "SELECT 1 FROM {DOCUMENTS} WHERE id = ?1 \
                 UNION ALL SELECT 1 FROM {JSON_DOCUMENTS} WHERE id = ?1 LIMIT 1"
            ))
            .map_err(failed)?
            .exists([ObjectKey::of(id).value()])
            .map_err(failed)
    }

    pub(crate) fn object(&self, id: ObjectId) -> Result<Option<Vec<u8>>, StorageError> {
        let mut statement = self
            .transaction
            .prepare_cached(&format!(
                "SELECT body FROM {DOCUMENTS} WHERE id = ?1 \
                 UNION ALL SELECT body FROM {JSON_DOCUMENTS} WHERE id = ?1"
            ))
            .map_err(failed)?;
        let mut found = statement
            .query([ObjectKey::of(id).value()])
            .map_err(failed)?;
        match found.next().map_err(failed)? {
            Some(row) => Ok(Some(Body::decode(row.get_ref(0).map_err(failed)?)?)),
            None => Ok(None),
        }
    }

    /// Every body asked for, one slot each and in the order asked.
    ///
    /// A missing object keeps its empty slot and an id asked for twice answers
    /// twice, because the answer is positional and a caller pairs by index.
    ///
    /// The body moves into the last position its id was asked at and is copied
    /// only for the earlier ones, so a scan, whose ids are all distinct, never
    /// copies a payload at all.
    pub(crate) fn objects(&self, ids: &[ObjectId]) -> Result<Vec<Option<Vec<u8>>>, StorageError> {
        if let [only] = ids {
            return Ok(vec![self.object(*only)?]);
        }

        let mut positions: BTreeMap<ObjectKey, Vec<usize>> = BTreeMap::new();
        for (at, id) in ids.iter().enumerate() {
            positions.entry(ObjectKey::of(*id)).or_default().push(at);
        }

        let wanted: Vec<ObjectKey> = positions.keys().copied().collect();
        let mut held: BTreeMap<ObjectKey, Vec<u8>> = BTreeMap::new();
        for batch in wanted.chunks(PER_STATEMENT) {
            self.gather(batch, &mut held)?;
        }

        let mut answered = vec![None; ids.len()];
        for (key, slots) in positions {
            let Some(body) = held.remove(&key) else {
                continue;
            };
            let Some((last, earlier)) = slots.split_last() else {
                continue;
            };
            for at in earlier {
                answered[*at] = Some(body.clone());
            }
            answered[*last] = Some(body);
        }
        Ok(answered)
    }

    /// The bodies of one batch of ids, into the map that is collecting them.
    ///
    /// Prepared rather than cached: the placeholder list is as wide as the
    /// batch, so caching it would fill a sixteen entry cache with shapes that
    /// are used once and evict the per-index statements that are used always.
    fn gather(
        &self,
        batch: &[ObjectKey],
        into: &mut BTreeMap<ObjectKey, Vec<u8>>,
    ) -> Result<(), StorageError> {
        let slots = vec!["?"; batch.len()].join(", ");
        let mut statement = self
            .transaction
            .prepare(&format!(
                "SELECT id, body FROM {DOCUMENTS} WHERE id IN ({slots}) \
                 UNION ALL SELECT id, body FROM {JSON_DOCUMENTS} WHERE id IN ({slots})"
            ))
            .map_err(failed)?;
        let bound: Vec<i64> = batch
            .iter()
            .chain(batch.iter())
            .map(|key| key.value())
            .collect();
        let mut found = statement.query(params_from_iter(bound)).map_err(failed)?;
        while let Some(row) = found.next().map_err(failed)? {
            let id: i64 = row.get(0).map_err(failed)?;
            let body = Body::decode(row.get_ref(1).map_err(failed)?)?;
            into.insert(ObjectKey::read(id)?, body);
        }
        Ok(())
    }
}

/// One batch being judged, before anything has moved.
///
/// Everything that could refuse the batch is asked through this, and nothing
/// that writes is reachable from it. `admits` consumes it, so the only way to
/// hold a [`Writing`] is to have judged first.
pub(crate) struct Judging<'a>(Held<'a>);

/// The batch admitted, and the only thing in this engine that writes.
///
/// Committing consumes it. Dropping instead rolls back, which is what every
/// early return relies on.
pub(crate) struct Writing<'a>(Held<'a>);

/// What judging a batch answered.
pub(crate) enum Verdict<'a> {
    /// An assertion no longer holds. Nothing has moved, and the transaction
    /// rolls back as this drops.
    Conflict,
    Admitted(Writing<'a>),
}

impl<'a> Judging<'a> {
    /// The store held still to judge one batch and write it.
    ///
    /// Immediate because the method split has already decided it: an operation
    /// with nothing staged never reaches `apply`, so this is the only writer.
    pub(crate) fn of(open: &'a mut Open) -> Result<Self, StorageError> {
        let (db, catalog) = open.parts();
        let transaction = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(failed)?;
        Ok(Self(Held {
            transaction,
            catalog,
        }))
    }

    pub(crate) fn layout(&self, index: IndexId) -> Result<&Layout, StorageError> {
        self.0.layout(index)
    }

    pub(crate) fn every_row(
        &self,
        index: IndexId,
        range: &KeyRange,
    ) -> Result<Vec<Row>, StorageError> {
        self.0.every_row(index, range)
    }

    pub(crate) fn has_object(&self, id: ObjectId) -> Result<bool, StorageError> {
        self.0.has_object(id)
    }

    /// The same transaction, now allowed to write.
    pub(crate) fn admitted(self) -> Verdict<'a> {
        Verdict::Admitted(Writing(self.0))
    }
}

impl Writing<'_> {
    pub(crate) fn commit(self) -> Result<(), StorageError> {
        self.0.transaction.commit().map_err(failed)
    }

    pub(crate) fn catalog(&self) -> &Catalog {
        self.0.catalog()
    }

    pub(crate) fn layout(&self, index: IndexId) -> Result<&Layout, StorageError> {
        self.0.layout(index)
    }

    /// Retires every row of the range, whatever object it names.
    ///
    /// Unlike a read range this one is exact: answering with more is data loss.
    pub(crate) fn retire(&self, delete: &RangeDelete) -> Result<(), StorageError> {
        let layout = self.layout(delete.index)?;
        let retire = layout.delete(layout.predicate(&delete.range)?)?;
        self.execute(&retire)
    }

    pub(crate) fn file_row(&self, entry: &IndexEntry) -> Result<(), StorageError> {
        let layout = self.layout(entry.index)?;
        let insert = layout.insert(&entry.columns, ObjectKey::of(entry.object))?;
        self.execute(&insert)
    }

    /// Runs a statement that only writes.
    ///
    /// Cached, because every statement reaching here has a shape the catalog
    /// decided rather than one a caller's arguments did.
    pub(crate) fn execute(&self, statement: &Statement) -> Result<(), StorageError> {
        self.0
            .transaction
            .prepare_cached(statement.sql())
            .map_err(failed)?
            .execute(params_from_iter(statement.values()))
            .map_err(failed)?;
        Ok(())
    }

    /// Files an object, which is only ever an insert.
    ///
    /// A duplicate is caught before this runs, so a conflict here would mean
    /// two entries in one batch naming the same id.
    pub(crate) fn file_object(&self, id: ObjectId, body: &[u8]) -> Result<(), StorageError> {
        let table = Body::of(body).table();
        self.0
            .transaction
            .prepare_cached(&format!("INSERT INTO {table} (id, body) VALUES (?, ?)"))
            .map_err(failed)?
            .execute(rusqlite::params![
                ObjectKey::of(id).value(),
                Body::encode(body)
            ])
            .map_err(failed)?;
        Ok(())
    }
}
