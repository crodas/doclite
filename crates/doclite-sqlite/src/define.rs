//! Declaring one home per index.
//!
//! The one thing that cannot go through a `Held`: it declares into the catalog
//! and so needs it mutably, where everything else only reads it.
//!
//! Called again with the same indexes on every open, so every statement here
//! tolerates repetition. What it does not tolerate is a second declaration that
//! disagrees, which `Catalog::declare` refuses.

use doclite_core::{IndexSchema, StorageError};

use crate::connection::{failed, Open};

/// Objects are inserted and never rewritten, so these tables are only ever
/// appended to and a primary-key conflict is how that rule is enforced within
/// one of them. Across the two it is `has_object`, which looks in both before
/// anything is filed.
///
/// Two tables rather than one column holding two storage classes, so each
/// `body` is exactly one kind. The JSON table is the legible half: a record
/// written by `#[storage(json)]` lands there as a text value, so `sqlite3`
/// prints the document and `json_extract` reads a field out of it in place.
const DOCUMENT_TABLES: &str = "\
    CREATE TABLE IF NOT EXISTS doclite_document (\
      id INTEGER PRIMARY KEY, body BLOB NOT NULL) WITHOUT ROWID;\
    CREATE TABLE IF NOT EXISTS doclite_json_document (\
      id INTEGER PRIMARY KEY, body TEXT NOT NULL) WITHOUT ROWID;";

pub(crate) fn define(open: &mut Open, indexes: &[IndexSchema]) -> Result<(), StorageError> {
    let (db, catalog) = open.parts();
    let transaction = db.transaction().map_err(failed)?;

    transaction.execute_batch(DOCUMENT_TABLES).map_err(failed)?;
    for schema in indexes {
        let table = catalog.declare(schema)?.create_table();
        transaction.execute_batch(&table).map_err(failed)?;
    }

    transaction.commit().map_err(failed)
}
