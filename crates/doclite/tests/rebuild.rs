//! That an index declared over records that are already filed gets their rows.
//!
//! These open a file rather than `SqliteEngine::in_memory`, so they do not go
//! through `on_every_engine!`. A rebuild is a thing a store notices about an
//! earlier run, and `under::Sqlite` is in-memory, so there is no earlier run to
//! notice; the memory engine keeps nothing at all and truthfully never reports
//! anything outstanding.
//!
//! The derive names an index after the snake-cased struct ident and nothing
//! else, so `v1::Note` and `v2::Note` declare the *same* index names at
//! different shapes. That is what lets one test file play both versions of a
//! schema.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use doclite::doclite_core::{
    Answers, Applied, Batch, Defined, Engine, IndexId, IndexSchema, KeyRange, RangeDelete, Reads,
    StorageError, ToOrdBytes,
};
use doclite::{records, Connection, Document, DocumentError, DocumentId, Spawner};
use doclite_sqlite::SqliteEngine;
use serde::{Deserialize, Serialize};

/// The schema before the change: a slug nobody may share, and nothing else.
mod v1 {
    use super::*;

    #[derive(Clone, Debug, Document, Serialize, Deserialize)]
    pub struct Note {
        #[unique]
        pub slug: String,
        pub shelf: String,
    }
}

/// The same record with `shelf` now indexed, which is the whole change.
mod v2 {
    use super::*;

    #[derive(Clone, Debug, Document, Serialize, Deserialize)]
    pub struct Note {
        #[unique]
        pub slug: String,
        #[index]
        pub shelf: String,
    }
}

/// The same record again, with the shelf claimed as unique.
mod v3 {
    use super::*;

    #[derive(Clone, Debug, Document, Serialize, Deserialize)]
    pub struct Note {
        #[unique]
        pub slug: String,
        #[unique]
        pub shelf: String,
    }
}

fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let at = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "doclite-rebuild-{}-{at}.sqlite",
        std::process::id()
    ))
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

/// Files these notes under the schema that does not index the shelf.
async fn before(path: &Path, notes: &[(&str, &str)]) -> Result<(), DocumentError> {
    let engine = SqliteEngine::open(path)?;
    let conn = Connection::open(engine, records![v1::Note]).await?;
    for (slug, shelf) in notes {
        conn.insert(v1::Note {
            slug: (*slug).to_string(),
            shelf: (*shelf).to_string(),
        })
        .await?;
    }
    Ok(())
}

/// A spawner that keeps the work instead of running it, so a test can decide
/// when the rebuild happens and watch what a reader does meanwhile.
#[derive(Default)]
struct Deferred {
    held: Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>,
}

impl Deferred {
    fn taken(&self) -> Vec<Pin<Box<dyn Future<Output = ()> + Send>>> {
        let mut held = self.held.lock().expect("the spawner is not poisoned");
        std::mem::take(&mut held)
    }
}

impl Spawner for &'static Deferred {
    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + Send>>) {
        self.held
            .lock()
            .expect("the spawner is not poisoned")
            .push(work);
    }
}

/// A spawner that throws the work away, which is what a runtime shutting down
/// looks like from here.
struct Dropped;

impl Spawner for Dropped {
    fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + Send>>) {
        drop(work);
    }
}

/// The headline. Three notes filed under a schema with no index on `shelf`,
/// reopened under one that has it, and the new index answers for all three.
#[tokio::test]
async fn an_index_added_to_an_existing_store_answers_for_every_record() -> Result<(), DocumentError>
{
    let path = scratch();
    remove(&path);

    before(
        &path,
        &[("one", "left"), ("two", "left"), ("three", "right")],
    )
    .await?;

    let engine = SqliteEngine::open(&path)?;
    let conn = Connection::open(engine, records![v2::Note]).await?;

    let left = v2::Note::query(&conn)
        .by_shelf("left".to_string())
        .get_many()
        .await?;
    assert_eq!(left.len(), 2, "both notes on the left shelf");

    let right = v2::Note::query(&conn)
        .by_shelf("right".to_string())
        .get_many()
        .await?;
    assert_eq!(right.len(), 1);

    let mut slugs: Vec<String> = left.iter().map(|note| note.slug.clone()).collect();
    slugs.sort();
    assert_eq!(slugs, vec!["one".to_string(), "two".to_string()]);

    remove(&path);
    Ok(())
}

/// Nothing is scanned twice. Reopening after a rebuild answers the same, which
/// is what says the index was recorded as filled rather than refilled.
#[tokio::test]
async fn a_rebuilt_index_is_not_rebuilt_again() -> Result<(), DocumentError> {
    let path = scratch();
    remove(&path);

    before(&path, &[("one", "left")]).await?;

    for _ in 0..3 {
        let engine = SqliteEngine::open(&path)?;
        let conn = Connection::open(engine, records![v2::Note]).await?;
        let found = v2::Note::query(&conn)
            .by_shelf("left".to_string())
            .get_many()
            .await?;
        assert_eq!(found.len(), 1, "exactly one row, never a duplicate");
    }

    remove(&path);
    Ok(())
}

/// A record edited after the rebuild has one row and not two: the rebuild filed
/// the old revision's row and the edit retired it.
#[tokio::test]
async fn editing_a_record_after_a_rebuild_leaves_one_row() -> Result<(), DocumentError> {
    let path = scratch();
    remove(&path);

    before(&path, &[("one", "left")]).await?;

    let engine = SqliteEngine::open(&path)?;
    let conn = Connection::open(engine, records![v2::Note]).await?;

    let mut note = v2::Note::query(&conn)
        .by_shelf("left".to_string())
        .get_one()
        .await?;
    note.shelf = "right".to_string();
    conn.update(note).await?;

    assert!(
        v2::Note::query(&conn)
            .by_shelf("left".to_string())
            .get_many()
            .await?
            .is_empty(),
        "the old row was retired"
    );
    assert_eq!(
        v2::Note::query(&conn)
            .by_shelf("right".to_string())
            .get_many()
            .await?
            .len(),
        1
    );

    remove(&path);
    Ok(())
}

/// The one that matters most. A `#[unique]` index declared over data that
/// already holds two of a key cannot be filled: an index carries no constraint,
/// so filing both rows would leave two documents claiming one key with nothing
/// afterwards able to tell. It has to be a refusal naming the index.
#[tokio::test]
async fn a_unique_index_over_existing_duplicates_is_refused() -> Result<(), DocumentError> {
    let path = scratch();
    remove(&path);

    before(&path, &[("one", "same"), ("two", "same")]).await?;

    let engine = SqliteEngine::open(&path)?;
    let refused = Connection::open(engine, records![v3::Note]).await;

    match refused {
        Err(DocumentError::Duplicate { index }) => {
            assert_eq!(index, "note_by_shelf", "the index that could not be filled")
        }
        Err(other) => panic!("a taken key, not {other}"),
        Ok(_) => panic!("two documents cannot both hold one unique shelf"),
    }

    remove(&path);
    Ok(())
}

/// A read of an index that is still being filled waits, and answers in full
/// once it is. The spawner holds the work, so the reader is parked for as long
/// as this test chooses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_of_an_unfilled_index_waits_for_it() -> Result<(), DocumentError> {
    static DEFERRED: Deferred = Deferred {
        held: Mutex::new(Vec::new()),
    };

    let path = scratch();
    remove(&path);

    before(&path, &[("one", "left"), ("two", "left")]).await?;

    let engine = SqliteEngine::open(&path)?;
    let conn = Connection::spawning(engine, records![v2::Note], &DEFERRED).await?;

    let reader = tokio::spawn(async move {
        v2::Note::query(&conn)
            .by_shelf("left".to_string())
            .get_many()
            .await
    });

    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert!(
        !reader.is_finished(),
        "the read is parked while the index is unfilled"
    );

    for work in DEFERRED.taken() {
        work.await;
    }

    let found = reader
        .await
        .expect("the reader task did not panic")
        .expect("the read answered");
    assert_eq!(found.len(), 2, "every row, not the ones filed so far");

    remove(&path);
    Ok(())
}

/// A read of an index nobody is filling is not slowed down by one that is
/// being filled. This is the whole reason the gate is per index.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_of_a_filled_index_does_not_wait() -> Result<(), DocumentError> {
    static DEFERRED: Deferred = Deferred {
        held: Mutex::new(Vec::new()),
    };

    let path = scratch();
    remove(&path);

    before(&path, &[("one", "left")]).await?;

    let engine = SqliteEngine::open(&path)?;
    let conn = Connection::spawning(engine, records![v2::Note], &DEFERRED).await?;

    let found = v2::Note::query(&conn)
        .by_slug("one".to_string())
        .get_many()
        .await?;
    assert_eq!(
        found.len(),
        1,
        "the slug index was never behind, so reading it does not wait"
    );

    for work in DEFERRED.taken() {
        work.await;
    }

    remove(&path);
    Ok(())
}

/// A spawner that drops the work leaves an index nothing will ever fill. The
/// read has to be told rather than left waiting for something that is not
/// coming.
#[tokio::test]
async fn an_abandoned_rebuild_tells_the_reader_rather_than_hanging() -> Result<(), DocumentError> {
    let path = scratch();
    remove(&path);

    before(&path, &[("one", "left")]).await?;

    let engine = SqliteEngine::open(&path)?;
    let conn = Connection::spawning(engine, records![v2::Note], Dropped).await?;

    match v2::Note::query(&conn)
        .by_shelf("left".to_string())
        .get_many()
        .await
    {
        Err(DocumentError::Rebuilding { index }) => {
            assert_eq!(index, doclite::doclite_core::IndexId("note_by_shelf"))
        }
        Err(other) => panic!("the rebuild went away, not {other}"),
        Ok(_) => panic!("nothing filled the index, so nothing may answer from it"),
    }

    remove(&path);
    Ok(())
}

/// Opening over schemas alone cannot fill anything, so it refuses rather than
/// parking every read of the index forever.
#[tokio::test]
async fn opening_over_schemas_alone_refuses_what_it_cannot_fill() -> Result<(), DocumentError> {
    let path = scratch();
    remove(&path);

    before(&path, &[("one", "left")]).await?;

    let engine = SqliteEngine::open(&path)?;
    let refused = Connection::over(engine, &<v2::Note as doclite::Record>::schemas()).await;

    match refused {
        Err(DocumentError::NoRebuilder { index }) => {
            assert_eq!(index, doclite::doclite_core::IndexId("note_by_shelf"))
        }
        Err(other) => panic!("nothing could fill it, not {other}"),
        Ok(_) => panic!("a handle with no record type cannot fill an index"),
    }

    remove(&path);
    Ok(())
}

/// Adding a record type to a store that already holds a different one. Every
/// index of the new type is missing rows because the type is new, there is
/// nothing to walk, and this is the most ordinary migration there is.
#[tokio::test]
async fn adding_a_record_type_to_an_existing_store_opens() -> Result<(), DocumentError> {
    #[derive(Clone, Debug, Document, Serialize, Deserialize)]
    struct Mark {
        #[index]
        note: String,
    }

    let path = scratch();
    remove(&path);

    before(&path, &[("one", "left")]).await?;

    let engine = SqliteEngine::open(&path)?;
    let conn = Connection::open(engine, records![v1::Note, Mark]).await?;

    conn.insert(Mark {
        note: "one".to_string(),
    })
    .await?;
    assert_eq!(
        Mark::query(&conn)
            .by_note("one".to_string())
            .get_many()
            .await?
            .len(),
        1
    );

    remove(&path);
    Ok(())
}

/// More records than one page holds, so the resume cursor is exercised rather
/// than assumed. A cursor that did not advance would loop; one that advanced
/// too far would silently leave rows unfiled, and a count is what catches that.
#[tokio::test]
async fn a_rebuild_pages_through_more_records_than_one_page_holds() -> Result<(), DocumentError> {
    let path = scratch();
    remove(&path);

    let many: Vec<(String, String)> = (0..600)
        .map(|at| (format!("slug-{at:04}"), format!("shelf-{}", at % 3)))
        .collect();

    let engine = SqliteEngine::open(&path)?;
    let conn = Connection::open(engine, records![v1::Note]).await?;
    for (slug, shelf) in &many {
        conn.insert(v1::Note {
            slug: slug.clone(),
            shelf: shelf.clone(),
        })
        .await?;
    }
    drop(conn);

    let engine = SqliteEngine::open(&path)?;
    let conn = Connection::open(engine, records![v2::Note]).await?;

    let mut total = 0;
    for shelf in 0..3 {
        let found = v2::Note::query(&conn)
            .by_shelf(format!("shelf-{shelf}"))
            .limit(1000)
            .get_many()
            .await?;
        total += found.len();
    }
    assert_eq!(total, 600, "every record across every page was filed once");

    remove(&path);
    Ok(())
}

/// A rebuild that refuses tells the reader why, rather than reporting that it
/// went away.
///
/// The eager path answers `Duplicate` because the refusal is its return value.
/// A spawned rebuild has no caller to return to, so the reason has to survive
/// on the gate, and collapsing it into abandonment is what made these two
/// doors disagree about one failure.
#[tokio::test]
async fn a_refused_spawned_rebuild_reports_the_reason_not_abandonment() -> Result<(), DocumentError>
{
    static DEFERRED: Deferred = Deferred {
        held: Mutex::new(Vec::new()),
    };

    let path = scratch();
    remove(&path);

    before(&path, &[("one", "same"), ("two", "same")]).await?;

    let engine = SqliteEngine::open(&path)?;
    let conn = Connection::spawning(engine, records![v3::Note], &DEFERRED).await?;

    for work in DEFERRED.taken() {
        work.await;
    }

    match v3::Note::query(&conn)
        .by_shelf("same".to_string())
        .get_many()
        .await
    {
        Err(DocumentError::Duplicate { index }) => {
            assert_eq!(index, "note_by_shelf", "the index that could not be filled")
        }
        Err(other) => panic!("a taken key, not {other}"),
        Ok(_) => panic!("the index could not be filled, so nothing may answer from it"),
    }

    remove(&path);
    Ok(())
}

/// An engine that retires one document's generated-index row just before
/// forwarding the first batch it is given.
///
/// `Engine::apply` is called by the executor rather than from inside the SQLite
/// connection's job, so a wrapper may apply a batch of its own first without
/// queueing behind the one it is about to forward. That is what makes the
/// window between the backfill's scan and its commit reachable from a test at
/// all.
///
/// Retiring the row is what `require_current` asks about, so it is the same
/// thing a concurrent writer does to that assertion: the revision the backfill
/// scanned is no longer the live one.
struct Interpose<E> {
    inner: E,
    retire: Mutex<Option<DocumentId>>,
}

impl<E> Interpose<E> {
    fn new(inner: E, document: DocumentId) -> Self {
        Self {
            inner,
            retire: Mutex::new(Some(document)),
        }
    }
}

#[async_trait]
impl<E: Engine> Engine for Interpose<E> {
    async fn define(&self, indexes: &[IndexSchema]) -> Result<Defined, StorageError> {
        self.inner.define(indexes).await
    }

    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        self.inner.read(reads).await
    }

    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError> {
        let armed = {
            let mut retire = self.retire.lock().expect("the interposer is not poisoned");
            retire.take()
        };

        if let Some(document) = armed {
            let delete = [RangeDelete {
                index: IndexId("note_latest"),
                range: KeyRange::Prefix(vec![document.to_ord_bytes()]),
            }];
            self.inner
                .apply(&Batch::Writes {
                    asserts: &[],
                    objects: &[],
                    delete: &delete,
                    insert: &[],
                })
                .await?;
        }

        self.inner.apply(batch).await
    }
}

/// A document superseded between the backfill's scan and its commit is dropped
/// from the page, and the rest are filed.
///
/// This is the branch the per-document `require_current` exists for. Without
/// it the backfill would file a row naming a revision that is no longer live,
/// and `Connection::select` would hand that dead revision back as if it were
/// current.
///
/// The retired document answers through nothing afterwards, which is correct
/// rather than incidental: it no longer has a live revision for a row to name.
#[tokio::test]
async fn a_document_superseded_mid_rebuild_is_dropped_from_the_page() -> Result<(), DocumentError> {
    let path = scratch();
    remove(&path);

    let moved = {
        let engine = SqliteEngine::open(&path)?;
        let conn = Connection::open(engine, records![v1::Note]).await?;
        let mut filed = Vec::new();
        for slug in ["one", "two", "three"] {
            filed.push(
                conn.insert(v1::Note {
                    slug: slug.to_string(),
                    shelf: "left".to_string(),
                })
                .await?
                .id(),
            );
        }
        filed[1]
    };

    let engine = Interpose::new(SqliteEngine::open(&path)?, moved);
    let conn = Connection::open(engine, records![v2::Note]).await?;

    let found = v2::Note::query(&conn)
        .by_shelf("left".to_string())
        .limit(10)
        .get_many()
        .await?;

    assert_eq!(
        found.len(),
        2,
        "the two whose revisions were still live were filed"
    );
    assert!(
        !found.iter().any(|note| note.id() == moved),
        "no row was filed for a revision that stopped being current"
    );

    remove(&path);
    Ok(())
}
