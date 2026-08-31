//! That an object written as text is text in the database.
//!
//! The point of `#[storage(json)]` is a store somebody can read without this
//! crate, and that only holds if the bytes reach SQLite as a text value:
//! `json_extract` refuses a blob, and `sqlite3` prints one as hex. Nothing in
//! the conformance suite can see the difference, because it only ever asks this
//! engine what it wrote.
//!
//! The bodies here are written through the byte-level API, so they are the
//! document and nothing else. A record going through `doclite` carries a
//! revision envelope around it and is read at `$.body`, which is that crate's
//! to test.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use doclite_core::{key, Db, Index, ObjectId, Operation, Outcome, Query, StorageError};
use doclite_sqlite::SqliteEngine;

static NOTES: Index<1> = Index::new("notes_by_slug");

const DOCUMENT: &str = r#"{"email":"a@b.c","age":24}"#;

/// Not UTF-8 at any offset, so nothing can mistake it for text.
const BINARY: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0xff, 0xfe, 0x00];

fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let at = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("doclite-bodies-{}-{at}.sqlite", std::process::id()))
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

/// Files one object under one slug, then lets the engine go so the file is
/// readable by something that is not this crate.
async fn written(path: &Path, id: ObjectId, slug: &str, body: &[u8]) -> Result<(), StorageError> {
    let db = Db::open(SqliteEngine::open(path)?, &[NOTES.schema()]).await?;
    let op = Operation::new()
        .put_object(id, body.to_vec())
        .put_index(&NOTES, key![slug], id);
    let Outcome::Committed(()) = db.run(&op).await? else {
        panic!("nothing here can be refused");
    };
    Ok(())
}

/// Which table the object landed in, and what SQLite says its body is.
fn held(path: &Path, id: ObjectId) -> (String, String) {
    let db = rusqlite::Connection::open(path).expect("the file is a database");
    db.query_row(
        "SELECT 'doclite_document', typeof(body) FROM doclite_document WHERE id = ?1 \
         UNION ALL \
         SELECT 'doclite_json_document', typeof(body) FROM doclite_json_document WHERE id = ?1",
        [id.0 as i64],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("the object is there")
}

#[tokio::test]
async fn a_text_body_is_text_to_sqlite_and_json_functions_reach_into_it() -> Result<(), StorageError>
{
    let path = scratch();
    remove(&path);

    let id = ObjectId(7);
    written(&path, id, "n-1", DOCUMENT.as_bytes()).await?;

    assert_eq!(
        held(&path, id),
        ("doclite_json_document".into(), "text".into()),
        "text goes to the JSON table, as a text value"
    );

    let db = rusqlite::Connection::open(&path).expect("the file is a database");
    let email: String = db
        .query_row(
            "SELECT json_extract(body, '$.email') FROM doclite_json_document WHERE id = ?",
            [id.0 as i64],
            |row| row.get(0),
        )
        .expect("json_extract reads a text body");
    assert_eq!(email, "a@b.c", "the document is queryable in place");

    remove(&path);
    Ok(())
}

#[tokio::test]
async fn a_body_that_is_not_text_stays_a_blob_rather_than_being_refused() -> Result<(), StorageError>
{
    let path = scratch();
    remove(&path);

    let id = ObjectId(9);
    written(&path, id, "n-1", BINARY).await?;
    assert_eq!(
        held(&path, id),
        ("doclite_document".into(), "blob".into()),
        "bytes that are not text stay bytes, in the other table"
    );

    remove(&path);
    Ok(())
}

/// The reason a read spans both tables: one database holds both kinds, and a
/// read of either has to answer with the bytes that went in.
#[tokio::test]
async fn both_classes_read_back_byte_identical_from_one_database() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let text = ObjectId(1);
    let binary = ObjectId(2);
    written(&path, text, "n-1", DOCUMENT.as_bytes()).await?;
    written(&path, binary, "n-2", BINARY).await?;

    let db = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;
    let read = |id| Query::object::<Vec<u8>>(id);
    let Outcome::Committed(Some(back)) = db.run(&read(text)).await? else {
        panic!("the text object is there");
    };
    assert_eq!(back, DOCUMENT.as_bytes());
    let Outcome::Committed(Some(back)) = db.run(&read(binary)).await? else {
        panic!("the binary object is there");
    };
    assert_eq!(back, BINARY);

    remove(&path);
    Ok(())
}
