//! A `for_each` written as statements instead of expanded row by row.
//!
//! `hints_are_hints` already proves a swept operation and an expanded one leave
//! the same store. What it cannot show is which happened, and an engine that
//! quietly declined every sweep would pass it. A taken sweep issues no scan, so
//! counting scans is what tells the two apart.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use doclite_core::{
    all, exact, key, prefix, Answers, Applied, Batch, Column, ColumnType, Db, Engine, Index,
    IndexSchema, ObjectId, Operation, Outcome, Plan, Query, ReadValue, Reads, Row, StorageError,
    Sweep,
};
use doclite_sqlite::SqliteEngine;

static PATIENTS: Index<2> = Index::new("sweep_patients");

/// The columns that hold an object id say so. A sweep copies the scanned row's
/// object straight across, which lands the right value only where the target
/// column is the integer that column is; declared as bytes, the engine declines
/// the sweep and the executor expands it instead.
static PATIENTS_V2: Index<2> =
    Index::typed("sweep_patients_v2", &[ColumnType::Bytes, ColumnType::I64]);
static NOTES: Index<1> = Index::typed("sweep_notes_by_patient", &[ColumnType::I64]);

/// Three columns, so a literal, a row column and the row's object each have
/// somewhere to land.
static TAGGED: Index<3> = Index::typed(
    "sweep_tagged",
    &[ColumnType::Bytes, ColumnType::Bytes, ColumnType::I64],
);

/// Counts the scans that reached the engine, and forwards everything.
struct Counted<E> {
    inner: E,
    scans: AtomicUsize,
}

impl<E> Counted<E> {
    fn new(inner: E) -> Self {
        Self {
            inner,
            scans: AtomicUsize::new(0),
        }
    }

    fn scans(&self) -> usize {
        self.scans.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl<E: Engine> Engine for Counted<E> {
    async fn define(&self, indexes: &[IndexSchema]) -> Result<(), StorageError> {
        self.inner.define(indexes).await
    }

    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        self.scans.fetch_add(reads.scans.len(), Ordering::Relaxed);
        self.inner.read(reads).await
    }

    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError> {
        self.inner.apply(batch).await
    }

    async fn plan(&self, wanted: &Plan) -> Result<Option<ReadValue>, StorageError> {
        self.inner.plan(wanted).await
    }

    async fn takes_sweep(&self, sweep: &Sweep) -> bool {
        self.inner.takes_sweep(sweep).await
    }
}

async fn open() -> Result<Db<Counted<SqliteEngine>>, StorageError> {
    let db = Db::open(
        Counted::new(SqliteEngine::in_memory()?),
        &[
            PATIENTS.schema(),
            PATIENTS_V2.schema(),
            NOTES.schema(),
            TAGGED.schema(),
        ],
    )
    .await?;

    let fill = Operation::new()
        .put_index(&PATIENTS, key!["clinic-a", 1u64], ObjectId(1))
        .put_index(&PATIENTS, key!["clinic-a", 2u64], ObjectId(2))
        .put_index(&PATIENTS, key!["clinic-b", 3u64], ObjectId(3))
        .put_index(&NOTES, key![ObjectId(1)], ObjectId(11))
        .put_index(&NOTES, key![ObjectId(2)], ObjectId(22))
        .put_index(&NOTES, key![ObjectId(3)], ObjectId(33));
    let Outcome::Committed(()) = db.run(&fill).await? else {
        panic!("nothing in the fill can be refused");
    };
    Ok(db)
}

async fn objects<E: Engine>(db: &Db<E>, index: &Index<2>) -> Result<Vec<ObjectId>, StorageError> {
    let Outcome::Committed(rows) = db.run(&Query::scan(index, all::<2>())).await? else {
        panic!("a scan is not something an assertion can refuse");
    };
    Ok(rows.iter().map(|row: &Row| row.object).collect())
}

/// The worked reindex from the spec, written by the engine rather than expanded.
#[tokio::test]
async fn a_reindex_is_written_without_reading_a_row() -> Result<(), StorageError> {
    let db = open().await?;
    let before = db.engine().scans();

    let reindex = Operation::new().for_each(
        Query::scan(&PATIENTS, prefix(key!["clinic-a"])).limit(1000),
        |row| Operation::new().put_index(&PATIENTS_V2, key![row.key(0), row.doc()], row.doc()),
    );
    assert!(db.run(&reindex).await?.is_committed());

    assert_eq!(
        db.engine().scans(),
        before,
        "the rows never left the database"
    );
    assert_eq!(
        objects(&db, &PATIENTS_V2).await?,
        [ObjectId(1), ObjectId(2)]
    );
    Ok(())
}

/// The worked cascade, as one delete against a subquery.
#[tokio::test]
async fn a_cascade_is_written_without_reading_a_row() -> Result<(), StorageError> {
    let db = open().await?;
    let before = db.engine().scans();

    let cascade = Operation::new().for_each(
        Query::scan(&PATIENTS, prefix(key!["clinic-a"])).limit(1000),
        |row| Operation::new().delete_index(&NOTES, exact(key![row.doc()])),
    );
    assert!(db.run(&cascade).await?.is_committed());

    assert_eq!(db.engine().scans(), before, "no row was fetched to delete");

    let Outcome::Committed(left) = db.run(&Query::scan(&NOTES, all::<1>())).await? else {
        panic!("a scan is not something an assertion can refuse");
    };
    let ids: Vec<ObjectId> = left.iter().map(|row: &Row| row.object).collect();
    assert_eq!(ids, [ObjectId(33)], "only clinic-b's note is left");
    Ok(())
}

/// A fixed range in a template is still the scan's to decide.
///
/// The literal branch collapses N identical deletes into one, which is right
/// for every N but zero: a scan naming no row expands the template none, so a
/// delete that fired anyway would empty the whole index. This is the branch
/// with the fixed range, taken by the engine, over a scan that finds nothing.
#[tokio::test]
async fn a_sweep_whose_scan_names_no_row_deletes_nothing() -> Result<(), StorageError> {
    let db = open().await?;
    let before = db.engine().scans();

    let cascade = Operation::new().for_each(
        Query::scan(&PATIENTS, prefix(key!["clinic-nobody"])).limit(1000),
        |_row| Operation::new().delete_index(&NOTES, all::<1>()),
    );
    assert!(db.run(&cascade).await?.is_committed());

    assert_eq!(db.engine().scans(), before, "the engine took the sweep");

    let Outcome::Committed(left) = db.run(&Query::scan(&NOTES, all::<1>())).await? else {
        panic!("a scan is not something an assertion can refuse");
    };
    let ids: Vec<ObjectId> = left.iter().map(|row: &Row| row.object).collect();
    assert_eq!(
        ids,
        [ObjectId(11), ObjectId(22), ObjectId(33)],
        "every note is still there"
    );
    Ok(())
}

/// A per-row precondition is not one statement, so the executor expands it and
/// the answer is the same.
#[tokio::test]
async fn a_template_that_asserts_is_expanded_instead() -> Result<(), StorageError> {
    let db = open().await?;
    let before = db.engine().scans();

    let guarded = Operation::new().for_each(
        Query::scan(&PATIENTS, prefix(key!["clinic-a"])).limit(1000),
        |row| {
            Operation::new()
                .require_absent(&PATIENTS_V2, all::<2>(), doclite_core::AbortCode::new(500))
                .put_index(&PATIENTS_V2, key![row.key(0), row.doc()], row.doc())
        },
    );
    let outcome = db.run(&guarded).await?;

    assert!(
        db.engine().scans() > before,
        "declining means the executor read the rows itself"
    );
    assert!(
        matches!(outcome, Outcome::Aborted(_)),
        "the second row finds what the first one filed"
    );
    Ok(())
}

/// The condition the whole design rests on: a step after the `for_each` could
/// observe what it wrote, so it is expanded and the overlay stays honest.
#[tokio::test]
async fn a_for_each_with_a_step_after_it_is_not_swept() -> Result<(), StorageError> {
    let db = open().await?;
    let before = db.engine().scans();

    let followed = Operation::new()
        .for_each(
            Query::scan(&PATIENTS, prefix(key!["clinic-a"])).limit(1000),
            |row| Operation::new().put_index(&PATIENTS_V2, key![row.key(0), row.doc()], row.doc()),
        )
        .put_index(&PATIENTS_V2, key!["clinic-z", 9u64], ObjectId(9));
    assert!(db.run(&followed).await?.is_committed());

    assert!(
        db.engine().scans() > before,
        "an operation with a second step is expanded"
    );
    assert_eq!(
        objects(&db, &PATIENTS_V2).await?,
        [ObjectId(1), ObjectId(2), ObjectId(9)]
    );
    Ok(())
}

/// Where a parameter-ordering mistake would be silent.
///
/// The template carries a literal beside its row slots and the scan carries a
/// range, so the statement binds the select list before the `WHERE`. Nothing
/// else says that order is right, and getting it wrong would file real rows
/// under wrong keys rather than fail.
#[tokio::test]
async fn a_literal_beside_row_slots_binds_before_the_range() -> Result<(), StorageError> {
    let db = open().await?;

    let tagged = Operation::new().for_each(
        Query::scan(&PATIENTS, prefix(key!["clinic-a"])).limit(1000),
        |row| {
            Operation::new().put_index(
                &TAGGED,
                key![Column::encode("tag"), row.key(0), row.doc()],
                row.doc(),
            )
        },
    );
    assert!(db.run(&tagged).await?.is_committed());

    let Outcome::Committed(rows) = db.run(&Query::scan(&TAGGED, all::<3>())).await? else {
        panic!("a scan is not something an assertion can refuse");
    };

    let ids: Vec<ObjectId> = rows.iter().map(|row: &Row| row.object).collect();
    assert_eq!(ids, [ObjectId(1), ObjectId(2)], "the range still selected");
    for row in &rows {
        let key = row.key.components();
        assert_eq!(key[0], b"tag".to_vec(), "the literal is the first column");
        assert_eq!(key[1], b"clinic-a".to_vec(), "the row column is the second");
    }
    Ok(())
}
