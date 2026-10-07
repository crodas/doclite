//! What shape each index table was built for, and whether its rows are all
//! there.
//!
//! `define` runs on every open with the whole catalog, and until this module
//! existed it answered every declaration with `CREATE TABLE IF NOT EXISTS`. A
//! declaration the store had never seen got an empty table, so a query through
//! it answered correctly-shaped nothing; one whose columns had moved kept the
//! table it already had, so the drift surfaced later as an arity nobody wrote.
//! `Catalog::declare` catches the second case within one process and cannot
//! catch either across a restart, because nothing was written down.
//!
//! So each index table has a row here recording the shape it was built for and
//! whether it is filled. ADR 0001 is what makes refilling the right answer
//! rather than refusing: an index can be rebuilt from the objects it names, so
//! a declaration that does not match what is stored costs a scan and never a
//! row.
//!
//! The shape is recorded as [`Storage`] per column rather than as the
//! `ColumnType` the caller declared, because `Storage` is exactly what decides
//! the bytes a key is held as. `Bool` and `U8` are both one unsigned byte, so
//! renaming one to the other needs no scan; `U8` to `U32` changes the width and
//! does.

use doclite_core::{IndexId, StorageError};
use rusqlite::{params, OptionalExtension, Transaction};

use crate::catalog::{DOCUMENTS, JSON_DOCUMENTS};
use crate::connection::failed;
use crate::storage::Storage;

/// The name, so `engine.rs` can mark one filled without rebuilding the SQL.
pub(crate) const INDEXES: &str = "doclite_index";

/// Where the shape of every index table is written down.
const VERSION_TABLE: &str = "\
    CREATE TABLE IF NOT EXISTS doclite_index (\
      name TEXT PRIMARY KEY, columns TEXT NOT NULL, filled INTEGER NOT NULL) WITHOUT ROWID";

/// A table one commit on this branch created and nothing ever read.
///
/// Dropped rather than left behind, so a database opened while that commit was
/// current does not keep an orphan nothing will ever write to again.
const ABANDONED: &str = "DROP TABLE IF EXISTS doclite_index_versions";

/// Whether an index holds every row it should.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// Every object that belongs in it has a row.
    Filled,
    /// Rows are missing, so nothing may read it until they are filed.
    Building,
}

/// Declares the table the shapes are recorded in.
///
/// Run inside the transaction `define` already owns, so the record of a table's
/// shape and the table itself are committed together or not at all.
pub(crate) fn declare(transaction: &Transaction<'_>) -> Result<(), StorageError> {
    transaction.execute_batch(VERSION_TABLE).map_err(failed)?;
    transaction.execute_batch(ABANDONED).map_err(failed)
}

/// Whether this table is already there.
///
/// What it answers is whether an earlier run built this index, which is not the
/// same question as whether an earlier run wrote down its shape: a database
/// opened by a version of this crate that had no [`INDEXES`] table has every
/// index table intact and no record of any of them. Asking the table rather
/// than only the record is what makes that upgrade free instead of a rebuild of
/// everything.
fn table_exists(transaction: &Transaction<'_>, table: &str) -> Result<bool, StorageError> {
    transaction
        .prepare_cached("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
        .map_err(failed)?
        .exists([table])
        .map_err(failed)
}

/// Whether this store holds any object at all.
///
/// Asked once per `define` rather than once per index. It is the difference
/// between a fresh store, where a declaration nobody has seen before is
/// complete the moment its table exists, and a store with history, where the
/// same declaration has every existing object to catch up on.
pub(crate) fn holds_objects(transaction: &Transaction<'_>) -> Result<bool, StorageError> {
    transaction
        .prepare_cached(&format!(
            "SELECT EXISTS(SELECT 1 FROM {DOCUMENTS}) \
             OR EXISTS(SELECT 1 FROM {JSON_DOCUMENTS})"
        ))
        .map_err(failed)?
        .query_row([], |row| row.get(0))
        .map_err(failed)
}

/// What this declaration means for the index table that is already there.
///
/// Five cases, and which one it is decides whether anything has to be scanned:
///
/// | recorded shape | table | store | answer |
/// |---|---|---|---|
/// | none | none | no objects | `Filled` |
/// | none | none | holds objects | `Building` |
/// | none | there | | `Filled` |
/// | same | | | whatever was recorded |
/// | moved | | | dropped, rebuilt, `Building` |
///
/// The third row is the upgrade: a database written before any of this existed
/// has its index tables and no record of them, and those tables were kept in
/// step by every write that touched them, so they are complete and saying so is
/// the truth rather than an optimism.
///
/// The fourth row keeps whatever was recorded, which is what lets a fill that
/// was interrupted carry on rather than be forgotten.
///
/// The fifth means every row in the table is keyed on bytes nothing will look
/// for again, so the table goes and the `CREATE` that follows builds it afresh.
///
/// The second row is the ordinary new index, and the first is the same
/// declaration against a store with nothing in it yet: an empty table is
/// already every row such a store can owe.
pub(crate) fn settle(
    transaction: &Transaction<'_>,
    index: IndexId,
    columns: &[Storage],
    table: &str,
    name: &str,
    holds_objects: bool,
) -> Result<State, StorageError> {
    let shape = written(columns);
    let recorded: Option<String> = transaction
        .prepare_cached(&format!("SELECT columns FROM {INDEXES} WHERE name = ?1"))
        .map_err(failed)?
        .query_row([index.0], |row| row.get(0))
        .optional()
        .map_err(failed)?;

    let state = match recorded {
        Some(held) if held == shape => recorded_state(transaction, index)?,
        Some(_moved) => {
            transaction
                .execute_batch(&format!("DROP TABLE IF EXISTS {table}"))
                .map_err(failed)?;
            State::Building
        }
        None if table_exists(transaction, name)? => State::Filled,
        None if holds_objects => State::Building,
        None => State::Filled,
    };

    transaction
        .prepare_cached(&format!(
            "INSERT INTO {INDEXES} (name, columns, filled) VALUES (?1, ?2, ?3) \
             ON CONFLICT(name) DO UPDATE SET columns = ?2, filled = ?3"
        ))
        .map_err(failed)?
        .execute(params![index.0, shape, state == State::Filled])
        .map_err(failed)?;

    Ok(state)
}

/// What was written down last time, for a shape that has not moved.
fn recorded_state(transaction: &Transaction<'_>, index: IndexId) -> Result<State, StorageError> {
    let filled: bool = transaction
        .prepare_cached(&format!("SELECT filled FROM {INDEXES} WHERE name = ?1"))
        .map_err(failed)?
        .query_row([index.0], |row| row.get(0))
        .map_err(failed)?;
    match filled {
        true => Ok(State::Filled),
        false => Ok(State::Building),
    }
}

/// The shape of a key, as one line that can be compared to the next run's.
///
/// Written out rather than hashed so that a mismatch can be read by a person
/// looking at the table with `sqlite3`, which is the same reason
/// `doclite_json_document` exists.
fn written(columns: &[Storage]) -> String {
    columns
        .iter()
        .map(|column| match column {
            Storage::Integer { bytes, signed } => {
                let sign = match signed {
                    true => 'i',
                    false => 'u',
                };
                format!("{sign}{bytes}")
            }
            Storage::Text => "text".to_string(),
            Storage::Blob => "blob".to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use doclite_core::ColumnType;

    use super::*;

    #[test]
    fn a_width_change_is_a_different_shape() {
        let narrow = written(&[Storage::Integer {
            bytes: 1,
            signed: false,
        }]);
        let wide = written(&[Storage::Integer {
            bytes: 4,
            signed: false,
        }]);
        assert_ne!(
            narrow, wide,
            "a key one byte wide is not one four bytes wide"
        );
    }

    /// The reason the shape is `Storage` and not `ColumnType`: these two
    /// declare the same bytes, so swapping one for the other needs no scan.
    #[test]
    fn two_column_types_that_store_the_same_bytes_are_the_same_shape() {
        let as_bool = written(&[Storage::of(&ColumnType::Bool)]);
        let as_byte = written(&[Storage::of(&ColumnType::U8)]);
        assert_eq!(as_bool, as_byte);
    }

    #[test]
    fn a_signed_column_is_not_its_unsigned_twin() {
        let signed = written(&[Storage::of(&ColumnType::I32)]);
        let unsigned = written(&[Storage::of(&ColumnType::U32)]);
        assert_ne!(signed, unsigned);
    }

    #[test]
    fn every_column_is_written_in_key_order() {
        let shape = written(&[Storage::Text, Storage::Blob, Storage::of(&ColumnType::U16)]);
        assert_eq!(shape, "text,blob,u2");
    }
}
