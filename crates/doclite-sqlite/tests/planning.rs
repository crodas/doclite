//! That the plan reaches the engine, and that declining it still answers.
//!
//! `hints_are_hints` already proves a planning engine and a declining one agree.
//! What it cannot show is that anything was planned at all: an engine that
//! quietly ignored the seam would pass it. These count what reached `read`.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use doclite_core::{
    all, exact, key, Answers, Applied, Batch, Db, Defined, Engine, Index, IndexSchema, ObjectId,
    Operation, Outcome, Plan, Query, ReadValue, Reads, StorageError,
};
use doclite_sqlite::SqliteEngine;

static NOTES: Index<1> = Index::new("planning_by_slug");

/// Counts the scans that reached the engine the ordinary way.
///
/// It forwards `plan`, which a decorator has to do on purpose: the trait's
/// default declines, so a wrapper that forgets turns the pushdown off without
/// changing an answer.
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
    async fn define(&self, indexes: &[IndexSchema]) -> Result<Defined, StorageError> {
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
}

/// Declines every plan, so the same query takes the ordinary path.
struct Flat<E>(Counted<E>);

#[async_trait]
impl<E: Engine> Engine for Flat<E> {
    async fn define(&self, indexes: &[IndexSchema]) -> Result<Defined, StorageError> {
        self.0.define(indexes).await
    }

    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        self.0.read(reads).await
    }

    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError> {
        self.0.apply(batch).await
    }
}

async fn filed<E: Engine>(engine: E) -> Result<Db<E>, StorageError> {
    let db = Db::open(engine, &[NOTES.schema()]).await?;
    let mut op = Operation::new();
    for at in 1..=20u64 {
        op = op
            .put_object(ObjectId(at), format!("note {at}").into_bytes())
            .put_index(&NOTES, key![format!("n-{at:02}")], ObjectId(at));
    }
    let Outcome::Committed(()) = db.run(&op).await? else {
        panic!("nothing here can be refused");
    };
    Ok(db)
}

/// The point of the whole seam: the rows stay where they are.
#[tokio::test]
async fn a_count_is_answered_without_a_scan() -> Result<(), StorageError> {
    let db = filed(Counted::new(SqliteEngine::in_memory()?)).await?;

    let Outcome::Committed(count) = db.run(&Query::count(&NOTES, all::<1>())).await? else {
        panic!("a count is not something an assertion can refuse");
    };

    assert_eq!(count, 20);
    assert_eq!(
        db.engine().scans(),
        0,
        "the count was answered where the rows are"
    );
    Ok(())
}

/// The same query against an engine that declines, which is what every engine
/// did before the seam existed.
#[tokio::test]
async fn declining_the_plan_still_answers_the_same_count() -> Result<(), StorageError> {
    let db = filed(Flat(Counted::new(SqliteEngine::in_memory()?))).await?;

    let Outcome::Committed(count) = db.run(&Query::count(&NOTES, all::<1>())).await? else {
        panic!("a count is not something an assertion can refuse");
    };

    assert_eq!(count, 20, "the same answer the planned path gave");
    assert!(
        db.engine().0.scans() > 0,
        "and it cost a scan of every row to get it"
    );
    Ok(())
}

/// Both halves of a zip come from one transaction rather than one call, which
/// is a stronger promise than the seam requires and the reason to plan it.
#[tokio::test]
async fn a_zip_is_answered_as_one_read() -> Result<(), StorageError> {
    let db = filed(Counted::new(SqliteEngine::in_memory()?)).await?;

    let query = Query::zip(
        Query::count(&NOTES, all::<1>()),
        Query::scan(&NOTES, exact(key!["n-01"])),
    );
    let Outcome::Committed((count, rows)) = db.run(&query).await? else {
        panic!("a zip is not something an assertion can refuse");
    };

    assert_eq!(count, 20);
    assert_eq!(rows.len(), 1);
    assert_eq!(db.engine().scans(), 0, "neither half went out as a scan");
    Ok(())
}
