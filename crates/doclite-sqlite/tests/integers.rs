//! Ids are integers on disk, not blobs.
//!
//! The point of `ObjectId` reserving its top bit (ADR 0014) is that a backend
//! whose integers are signed can hold one as an integer. Nothing else here
//! notices the difference: the conformance suite passes either way, and so does
//! every test that reads through this engine. What the promise buys is a
//! database somebody can open in `sqlite3` and read, so the storage class is
//! what this file asserts, against the file itself rather than through the API
//! that wrote it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use doclite_core::{all, key, ColumnType, Db, Index, ObjectId, Operation, Query, StorageError};
use doclite_sqlite::SqliteEngine;

/// Two columns that hold different things: a name, and a reference to another
/// object. The second is what a `Ref<T>` becomes once the derive has declared
/// it.
static NOTES: Index<2> = Index::typed("notes_by_owner", &[ColumnType::Str, ColumnType::I64]);

fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let at = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("doclite-int-{}-{at}.sqlite", std::process::id()))
}

fn remove(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut beside = path.to_path_buf().into_os_string();
        beside.push(suffix);
        if std::fs::remove_file(PathBuf::from(beside)).is_err() {
            continue;
        }
    }
}

/// What `sqlite3` would say each column is, read from the file directly.
fn kinds(path: &Path, sql: &str) -> Vec<String> {
    let db = rusqlite::Connection::open(path).expect("the database opens");
    let mut statement = db.prepare(sql).expect("the query prepares");
    let mut found = statement.query([]).expect("the query runs");
    let mut kinds = Vec::new();
    while let Some(row) = found.next().expect("a row") {
        let mut at = 0;
        while let Ok(kind) = row.get::<_, String>(at) {
            kinds.push(kind);
            at += 1;
        }
    }
    kinds
}

#[tokio::test]
async fn every_id_on_disk_is_an_integer() -> Result<(), StorageError> {
    let path = scratch();
    {
        let db = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;
        let filed = Operation::new()
            .put_object(ObjectId(7), b"a note".to_vec())
            .put_index(&NOTES, key!["ana", ObjectId(41)], ObjectId(7));
        assert!(db.run(&filed).await?.is_committed());
    }

    // Both object tables, because which one a body lands in depends on the
    // bytes and neither is allowed to hold its key as anything but an integer.
    assert_eq!(
        kinds(
            &path,
            "SELECT typeof(id) FROM doclite_document \
             UNION ALL SELECT typeof(id) FROM doclite_json_document"
        ),
        ["integer"],
        "an object table's primary key"
    );

    // Column one is the reference, column two is the row's object. Both hold an
    // id and neither should be a blob.
    let index = kinds(
        &path,
        "SELECT typeof(c0), typeof(c1), typeof(object) \
         FROM doclite_idx_notes_by_owner",
    );
    assert_eq!(
        index,
        ["text", "integer", "integer"],
        "a name, a reference, and the object the row points at"
    );

    remove(&path);
    Ok(())
}

/// The value survives the storage class, which is the half a `typeof` cannot
/// show.
#[tokio::test]
async fn an_id_stored_as_an_integer_reads_back_as_itself() -> Result<(), StorageError> {
    let path = scratch();
    let biggest = ObjectId(ObjectId::LIMIT - 1);
    {
        let db = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;
        let filed = Operation::new()
            .put_index(&NOTES, key!["ana", ObjectId(1)], ObjectId(1))
            .put_index(&NOTES, key!["ana", biggest], biggest);
        assert!(db.run(&filed).await?.is_committed());
    }

    let db = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;
    let rows = db
        .run(&Query::scan(&NOTES, all::<2>()))
        .await?
        .committed()
        .expect("a scan is not something an assertion can refuse");

    let ids: Vec<ObjectId> = rows.iter().map(|row| row.object).collect();
    assert_eq!(
        ids,
        [ObjectId(1), biggest],
        "the largest id an operation may name still sorts last and reads back"
    );

    remove(&path);
    Ok(())
}
