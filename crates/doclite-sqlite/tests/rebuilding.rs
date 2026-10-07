//! That a declaration is compared against what the last run stored.
//!
//! None of this is observable in one process. `define` runs on every open and
//! the thing it has to notice is a database written by an earlier run, so every
//! test here opens a file, drops the engine, and opens it again.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use doclite_core::{
    exact, key, ColumnType, Db, Index, IndexSchema, ObjectId, Operation, StorageError,
};
use doclite_sqlite::SqliteEngine;

static LATEST: Index<1> = Index::typed("notes_latest", &[ColumnType::U64]);
static BY_SLUG: Index<1> = Index::typed("notes_by_slug", &[ColumnType::Str]);
static BY_SHELF: Index<1> = Index::typed("notes_by_shelf", &[ColumnType::Str]);

/// The same index name at a different width, which is what a field changing
/// type looks like from here.
static WIDE_SHELF: Index<2> = Index::typed("notes_by_shelf", &[ColumnType::Str, ColumnType::U64]);

/// A path nothing else in this run will take.
fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let at = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "doclite-rebuilding-{}-{at}.sqlite",
        std::process::id()
    ))
}

/// Removes the database and the two files a write-ahead log leaves beside it.
fn remove(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut beside = path.to_path_buf().into_os_string();
        beside.push(suffix);
        if std::fs::remove_file(PathBuf::from(beside)).is_err() {
            continue;
        }
    }
}

/// Opens, hands the engine over, and says which indexes it reported.
async fn opened(path: &Path, schemas: &[IndexSchema]) -> Result<Vec<String>, StorageError> {
    let engine = SqliteEngine::open(path)?;
    let db = Db::open(engine, schemas).await?;
    Ok(db
        .building()
        .iter()
        .map(|index| index.0.to_string())
        .collect())
}

/// Files one object with a row in each index, so the store stops being empty.
async fn file(path: &Path, schemas: &[IndexSchema], id: u64) -> Result<(), StorageError> {
    let engine = SqliteEngine::open(path)?;
    let db = Db::open(engine, schemas).await?;
    let operation = Operation::new()
        .put_object(ObjectId(id), format!("note {id}").into_bytes())
        .put_index(&LATEST, key![id], ObjectId(id))
        .put_index(&BY_SLUG, key![format!("slug-{id}")], ObjectId(id));
    db.run(&operation).await?;
    Ok(())
}

/// What `doclite_index` holds for one index, so a test can assert the record
/// and not only what was reported.
fn recorded(path: &Path, index: &str) -> Option<(String, bool)> {
    let db = rusqlite::Connection::open(path).expect("the database opens");
    db.query_row(
        "SELECT columns, filled FROM doclite_index WHERE name = ?1",
        [index],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .ok()
}

/// Nothing is outstanding on a store with nothing in it, however many indexes
/// are declared: an empty table is already every row such a store can owe.
#[tokio::test]
async fn an_index_declared_on_an_empty_store_owes_nothing() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let building = opened(&path, &[LATEST.schema(), BY_SLUG.schema()]).await?;
    assert!(building.is_empty(), "{building:?}");

    remove(&path);
    Ok(())
}

/// The headline case. An index the store has never seen, declared against a
/// store that already holds objects, has every one of them to catch up on.
#[tokio::test]
async fn a_new_index_on_a_store_holding_objects_is_reported() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let first = [LATEST.schema(), BY_SLUG.schema()];
    file(&path, &first, 1).await?;

    let second = [LATEST.schema(), BY_SLUG.schema(), BY_SHELF.schema()];
    let building = opened(&path, &second).await?;
    assert_eq!(building, vec!["notes_by_shelf".to_string()]);

    remove(&path);
    Ok(())
}

/// Reopening with the catalog that is already recorded reports nothing, which
/// is what keeps an ordinary restart from scanning the whole store.
#[tokio::test]
async fn reopening_the_same_catalog_reports_nothing() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let schemas = [LATEST.schema(), BY_SLUG.schema()];
    file(&path, &schemas, 1).await?;

    let building = opened(&path, &schemas).await?;
    assert!(building.is_empty(), "{building:?}");

    remove(&path);
    Ok(())
}

/// A column that changed width makes every row in the table keyed on bytes
/// nothing will look for again, so the table goes and is built afresh.
#[tokio::test]
async fn a_column_that_changed_width_is_dropped_and_reported() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let narrow = [LATEST.schema(), BY_SLUG.schema(), BY_SHELF.schema()];
    file(&path, &narrow, 1).await?;
    let shape = recorded(&path, "notes_by_shelf").expect("the shape was recorded");

    let wide = [LATEST.schema(), BY_SLUG.schema(), WIDE_SHELF.schema()];
    let building = opened(&path, &wide).await?;
    assert_eq!(building, vec!["notes_by_shelf".to_string()]);

    let moved = recorded(&path, "notes_by_shelf").expect("the new shape was recorded");
    assert_ne!(moved.0, shape.0, "the recorded shape moved with the column");

    let db = rusqlite::Connection::open(&path).expect("the database opens");
    let columns: usize = db
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('doclite_idx_notes_by_shelf')",
            [],
            |row| row.get(0),
        )
        .expect("the table is there");
    assert_eq!(columns, 3, "two key columns and the object");

    remove(&path);
    Ok(())
}

/// Saying an index is filled survives the process, so the next open does not
/// scan it again.
#[tokio::test]
async fn an_index_reported_rebuilt_stays_rebuilt() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let first = [LATEST.schema(), BY_SLUG.schema()];
    file(&path, &first, 1).await?;

    let second = [LATEST.schema(), BY_SLUG.schema(), BY_SHELF.schema()];
    let engine = SqliteEngine::open(&path)?;
    let db = Db::open(engine, &second).await?;
    assert_eq!(db.building().len(), 1);
    db.rebuilt(BY_SHELF.id()).await?;
    drop(db);

    let building = opened(&path, &second).await?;
    assert!(building.is_empty(), "{building:?}");

    remove(&path);
    Ok(())
}

/// A rebuild that was interrupted has to be noticed again rather than
/// forgotten, or the index stays half filled with nothing left to say so.
#[tokio::test]
async fn an_unfinished_rebuild_is_reported_again() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let first = [LATEST.schema(), BY_SLUG.schema()];
    file(&path, &first, 1).await?;

    let second = [LATEST.schema(), BY_SLUG.schema(), BY_SHELF.schema()];
    assert_eq!(opened(&path, &second).await?.len(), 1);
    assert_eq!(
        opened(&path, &second).await?,
        vec!["notes_by_shelf".to_string()],
        "nothing said it was filled, so it still is not"
    );

    remove(&path);
    Ok(())
}

/// The upgrade path. A database written before any of this existed has its
/// index tables and no record of them, and those tables were kept in step by
/// every write that touched them, so they are complete. Rebuilding them all on
/// first open would be slow and would say nothing true.
#[tokio::test]
async fn an_index_table_with_no_record_is_taken_as_complete() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let schemas = [LATEST.schema(), BY_SLUG.schema()];
    file(&path, &schemas, 1).await?;

    let db = rusqlite::Connection::open(&path).expect("the database opens");
    db.execute_batch("DELETE FROM doclite_index")
        .expect("the record of every shape goes, the tables stay");
    drop(db);

    let building = opened(&path, &schemas).await?;
    assert!(
        building.is_empty(),
        "an intact table is complete even with nothing written down: {building:?}"
    );

    remove(&path);
    Ok(())
}

/// The table one commit created and nothing ever read does not survive an open.
#[tokio::test]
async fn the_abandoned_table_is_dropped() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let db = rusqlite::Connection::open(&path).expect("the database opens");
    db.execute_batch("CREATE TABLE doclite_index_versions (name TEXT PRIMARY KEY)")
        .expect("the orphan is created");
    drop(db);

    opened(&path, &[LATEST.schema(), BY_SLUG.schema()]).await?;

    let db = rusqlite::Connection::open(&path).expect("the database opens");
    let orphan: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master \
             WHERE type = 'table' AND name = 'doclite_index_versions')",
            [],
            |row| row.get(0),
        )
        .expect("sqlite_master answers");
    assert!(!orphan, "the orphan table is gone");

    remove(&path);
    Ok(())
}

/// Rows already in an index are still there after a reopen that reports a
/// different index as outstanding: noticing one index is behind must not
/// disturb another.
#[tokio::test]
async fn reporting_one_index_leaves_the_others_alone() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let first = [LATEST.schema(), BY_SLUG.schema()];
    file(&path, &first, 1).await?;

    let second = [LATEST.schema(), BY_SLUG.schema(), BY_SHELF.schema()];
    let engine = SqliteEngine::open(&path)?;
    let db = Db::open(engine, &second).await?;

    let held = Operation::new().require_count(&BY_SLUG, exact(key!["slug-1"]), 1, DID_NOT_HOLD);
    assert!(
        matches!(db.run(&held).await?, doclite_core::Outcome::Committed(())),
        "the row filed before the reopen is still there"
    );

    remove(&path);
    Ok(())
}

const DID_NOT_HOLD: doclite_core::AbortCode = doclite_core::AbortCode::new(900);
