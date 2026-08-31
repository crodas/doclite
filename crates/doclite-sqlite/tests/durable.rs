//! That the point of the engine holds: what was written is still there.
//!
//! The conformance suite runs in memory, so it exercises no file, no
//! write-ahead log and no fsync. This is the smallest test that does.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use doclite_core::{all, Db, Index, ObjectId, Operation, Outcome, Query, StorageError};
use doclite_sqlite::{Durability, SqliteEngine};

static NOTES: Index<1> = Index::new("notes_by_slug");

/// A path nothing else in this run will take.
fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let at = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("doclite-{}-{at}.sqlite", std::process::id()))
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

#[tokio::test]
async fn what_was_written_is_there_after_the_engine_is_gone() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let filed = ObjectId(7);
    {
        let db = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;
        let op = Operation::new()
            .put_object(filed, b"a note".to_vec())
            .put_index(&NOTES, doclite_core::key!["n-1"], filed);
        let Outcome::Committed(()) = db.run(&op).await? else {
            panic!("nothing here can be refused");
        };
    }

    let db = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;
    let Outcome::Committed(rows) = db.run(&Query::scan(&NOTES, all::<1>())).await? else {
        panic!("a scan is not something an assertion can refuse");
    };
    assert_eq!(rows.len(), 1, "the row outlived the engine that wrote it");
    assert_eq!(rows[0].object, filed);

    let Outcome::Committed(body) = db.run(&Query::object::<Vec<u8>>(filed)).await? else {
        panic!("an object read is not something an assertion can refuse");
    };
    assert_eq!(body, Some(b"a note".to_vec()));

    remove(&path);
    Ok(())
}

/// `define` runs on every open, so the second one must find its tables rather
/// than try to build them again.
#[tokio::test]
async fn opening_a_database_twice_declares_the_same_indexes() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;
    let db = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;

    let Outcome::Committed(rows) = db.run(&Query::scan(&NOTES, all::<1>())).await? else {
        panic!("a scan is not something an assertion can refuse");
    };
    assert!(rows.is_empty());

    remove(&path);
    Ok(())
}

/// More objects than one statement is allowed to bind, so the batching that
/// answers them is exercised rather than assumed.
///
/// A scan asking for objects is where a long list of ids actually comes from:
/// the executor collects one id per row it found.
#[tokio::test]
async fn a_scan_fetches_objects_past_one_statement() -> Result<(), StorageError> {
    let db = Db::open(SqliteEngine::in_memory()?, &[NOTES.schema()]).await?;

    let held: Vec<u64> = (1..=1200).collect();
    let mut op = Operation::new();
    for id in &held {
        op = op
            .put_object(ObjectId(*id), format!("body {id}").into_bytes())
            .put_index(
                &NOTES,
                doclite_core::key![format!("n-{id:05}")],
                ObjectId(*id),
            );
    }
    let Outcome::Committed(()) = db.run(&op).await? else {
        panic!("nothing here can be refused");
    };

    let Outcome::Committed(found) = db
        .run(&Query::scan(&NOTES, all::<1>()).with_objects::<Vec<u8>>())
        .await?
    else {
        panic!("a scan is not something an assertion can refuse");
    };

    assert_eq!(found.len(), held.len(), "one entry per row, across batches");
    assert_eq!(found[0].1, b"body 1".to_vec());
    assert_eq!(found[1199].1, b"body 1200".to_vec());
    assert_eq!(
        found[600].1,
        b"body 601".to_vec(),
        "a row from the middle batch keeps its own body"
    );
    Ok(())
}

/// A database that promises less still keeps what it was given.
///
/// This says the option is wired through, not that it is faster: nothing here
/// can honestly test what survives a power cut, which is the whole reason the
/// two answers are named for what they survive.
#[tokio::test]
async fn a_database_durable_only_against_a_crash_still_reopens() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let filed = ObjectId(3);
    {
        let engine = SqliteEngine::open_with(&path, Durability::ProcessCrash)?;
        let db = Db::open(engine, &[NOTES.schema()]).await?;
        let op = Operation::new()
            .put_object(filed, b"quicker".to_vec())
            .put_index(&NOTES, doclite_core::key!["n-2"], filed);
        let Outcome::Committed(()) = db.run(&op).await? else {
            panic!("nothing here can be refused");
        };
    }

    let db = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;
    let Outcome::Committed(body) = db.run(&Query::object::<Vec<u8>>(filed)).await? else {
        panic!("an object read is not something an assertion can refuse");
    };
    assert_eq!(body, Some(b"quicker".to_vec()));

    remove(&path);
    Ok(())
}

/// An id asked for twice, across more ids than a single statement binds.
///
/// Two rows naming one object is how a repeat reaches the fetch. The body moves
/// into the last position its id was asked at and is copied for the earlier
/// ones, so a repeat and a single occurrence take different paths and only this
/// says they agree.
#[tokio::test]
async fn an_object_named_twice_answers_twice_across_batches() -> Result<(), StorageError> {
    let db = Db::open(SqliteEngine::in_memory()?, &[NOTES.schema()]).await?;

    let held: Vec<u64> = (1..=1200).collect();
    let mut op = Operation::new();
    for id in &held {
        op = op
            .put_object(ObjectId(*id), format!("body {id}").into_bytes())
            .put_index(
                &NOTES,
                doclite_core::key![format!("n-{id:05}")],
                ObjectId(*id),
            );
    }
    // A second row, far from the first, naming an object the scan already asks
    // for. It lands in a different batch than the row that shares its object.
    op = op.put_index(&NOTES, doclite_core::key!["n-99999"], ObjectId(1));

    let Outcome::Committed(()) = db.run(&op).await? else {
        panic!("nothing here can be refused");
    };

    let Outcome::Committed(found) = db
        .run(&Query::scan(&NOTES, all::<1>()).with_objects::<Vec<u8>>())
        .await?
    else {
        panic!("a scan is not something an assertion can refuse");
    };

    assert_eq!(
        found.len(),
        held.len() + 1,
        "one entry per row, not per object"
    );
    assert_eq!(found[0].1, b"body 1".to_vec(), "the first row naming it");
    assert_eq!(
        found[held.len()].1,
        b"body 1".to_vec(),
        "and the last one, which is where the body moved rather than copied"
    );
    assert_eq!(
        found[1199].1,
        b"body 1200".to_vec(),
        "the others are untouched"
    );
    Ok(())
}

/// Two engines over one file see each other.
///
/// This is the write-ahead log doing what the design says it is for: a reader
/// and the writer stop blocking each other, and each engine has its own
/// connection on its own thread, so nothing here is shared but the file.
#[tokio::test]
async fn a_second_engine_reads_what_the_first_one_wrote() -> Result<(), StorageError> {
    let path = scratch();
    remove(&path);

    let writer = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;
    let reader = Db::open(SqliteEngine::open(&path)?, &[NOTES.schema()]).await?;

    // The reader is open before the write, so it cannot be seeing a snapshot it
    // took at startup.
    let Outcome::Committed(before) = reader.run(&Query::scan(&NOTES, all::<1>())).await? else {
        panic!("a scan is not something an assertion can refuse");
    };
    assert!(before.is_empty());

    let filed = ObjectId(11);
    let op = Operation::new()
        .put_object(filed, b"shared".to_vec())
        .put_index(&NOTES, doclite_core::key!["n-shared"], filed);
    let Outcome::Committed(()) = writer.run(&op).await? else {
        panic!("nothing here can be refused");
    };

    let Outcome::Committed(after) = reader.run(&Query::scan(&NOTES, all::<1>())).await? else {
        panic!("a scan is not something an assertion can refuse");
    };
    assert_eq!(after.len(), 1, "the reader sees the writer's commit");
    assert_eq!(after[0].object, filed);

    let Outcome::Committed(body) = reader.run(&Query::object::<Vec<u8>>(filed)).await? else {
        panic!("an object read is not something an assertion can refuse");
    };
    assert_eq!(body, Some(b"shared".to_vec()));

    drop(reader);
    drop(writer);
    remove(&path);
    Ok(())
}
