//! The SQLite engine against the definition of a correct engine.
//!
//! A fresh database per call, in memory, because the suite builds one per rule
//! and one rule builds two at once. `tests/durable.rs` is where a file is
//! opened; this is where the rules are.

use async_trait::async_trait;
use doclite_core::StorageError;
use doclite_sqlite::SqliteEngine;
use doclite_testkit::Fresh;

/// What the suite builds engines through.
struct Sqlite;

#[async_trait]
impl Fresh for Sqlite {
    type Engine = SqliteEngine;

    async fn fresh() -> Result<SqliteEngine, StorageError> {
        SqliteEngine::in_memory()
    }
}

doclite_testkit::conformance!(
    #[tokio::test]
    Sqlite
);
