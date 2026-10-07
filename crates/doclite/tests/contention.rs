//! Two writers at once, which nothing else here has ever done.
//!
//! The conflict machinery was already covered, but only by a fake: `stale.rs`
//! has an engine that reports `Applied::Conflict` on demand, and the executor
//! is driven against it on one thread. What none of that says is whether a
//! conflict happens when it should, because no test ever let two writers
//! collide.
//!
//! They can collide because `Db::run` reads and applies in two separate calls
//! on the engine. Everything a precondition saw was seen in the first call and
//! is re-judged in the second, and the gap between them is where another writer
//! fits. No second process is needed to open one, and no second engine: two
//! tasks on one connection are enough.
//!
//! Each scenario asserts on the outcome rather than on the interleaving, so it
//! means the same thing whichever order the scheduler picked.

use std::sync::Arc;

use doclite::doclite_core::{all, Outcome, Query, StorageError};
use doclite::{records, Column, Connection, Document, DocumentError, Records};
use serde::{Deserialize, Serialize};

mod under;
use under::{on_every_engine, Under};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Email(String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Slug(String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Member {
    #[unique]
    email: Email,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Counter {
    #[unique]
    slug: Slug,

    hits: u32,
}

/// Enough writers that the gap is wide, few enough that a serialized engine
/// still finishes quickly.
const WRITERS: usize = 8;

fn catalog() -> Records {
    records![Member, Counter]
}

/// The system clock rather than a pinned one, because reproducible bytes are
/// not the question here and one mint hands out ids to every task.
async fn open<U: Under>() -> Connection<U::Engine> {
    Connection::open(U::engine().await, catalog())
        .await
        .expect("the engine accepts its own schemas")
}

/// A unique key is claimed by exactly one writer, however many asked at once.
///
/// The absence check is a precondition, so every task can read "nobody holds
/// this" before any of them writes. What settles it is `apply` re-judging that
/// assertion against pre-batch state, and the losers are refusals rather than
/// conflicts: an operation the store has an answer for is not a race that can
/// be re-run.
async fn one_writer_takes_the_key<U: Under>() -> Result<(), DocumentError> {
    let conn = Arc::new(open::<U>().await);

    let mut writing = Vec::new();
    for _ in 0..WRITERS {
        let conn = Arc::clone(&conn);
        writing.push(tokio::spawn(async move {
            conn.insert(Member {
                email: Email("one@example.test".into()),
            })
            .await
        }));
    }

    let mut filed = 0;
    let mut refused = 0;
    for task in writing {
        match task.await.expect("no writer panicked") {
            Ok(_) => filed += 1,
            Err(DocumentError::Duplicate { index }) => {
                assert_eq!(
                    index, "member_by_email",
                    "refused by the index that holds it"
                );
                refused += 1;
            }
            Err(other) => panic!("neither filed nor refused: {other}"),
        }
    }

    assert_eq!(filed, 1, "one writer filed the record");
    assert_eq!(refused, WRITERS - 1, "every other writer was told why");
    Ok(())
}

/// An edit that lost is refused rather than lost.
///
/// The loser gets `Stale` and not a silent retry, and that is the distinction
/// worth having a test for. `Db::run` does re-walk on a conflict, but the walk
/// builds the same operation, which asserts the same superseded revision, so it
/// aborts on the second pass as well. Retrying is the caller's, against
/// whatever superseded it, and doing so is what makes the count come out right.
///
/// An exhausted retry budget is the same answer in a different word, so it is
/// retried too. Nothing promises that sixteen walks are enough for any number
/// of writers, and treating `Conflict` as fatal would assert a guarantee the
/// design does not make.
async fn no_update_is_lost<U: Under>() -> Result<(), DocumentError> {
    let conn = Arc::new(open::<U>().await);
    conn.insert(Counter {
        slug: Slug("hits".into()),
        hits: 0,
    })
    .await?;

    let mut writing = Vec::new();
    for _ in 0..WRITERS {
        let conn = Arc::clone(&conn);
        writing.push(tokio::spawn(async move {
            loop {
                let mut head = Counter::query(conn.as_ref())
                    .by_slug(Slug("hits".into()))
                    .get_one()
                    .await?;
                head.hits += 1;
                match conn.update(head).await {
                    Ok(_) => return Ok::<(), DocumentError>(()),
                    Err(DocumentError::Stale) => continue,
                    Err(DocumentError::Storage(StorageError::Conflict { .. })) => continue,
                    Err(other) => return Err(other),
                }
            }
        }));
    }

    for task in writing {
        task.await.expect("no writer panicked")?;
    }

    let counted = Counter::query(conn.as_ref())
        .by_slug(Slug("hits".into()))
        .get_one()
        .await?;

    assert_eq!(
        counted.hits, WRITERS as u32,
        "every increment is in the total"
    );
    Ok(())
}

/// Writers that want nothing from each other all get what they asked for.
///
/// The uninteresting half, and the one that would catch the worst bug: ids
/// minted concurrently that collide, or a batch that lands on top of another
/// rather than beside it.
async fn concurrent_inserts_all_land<U: Under>() -> Result<(), DocumentError> {
    let conn = Arc::new(open::<U>().await);

    let mut writing = Vec::new();
    for at in 0..WRITERS {
        let conn = Arc::clone(&conn);
        writing.push(tokio::spawn(async move {
            conn.insert(Member {
                email: Email(format!("{at}@example.test")),
            })
            .await
        }));
    }

    let mut filed = Vec::new();
    for task in writing {
        filed.push(task.await.expect("no writer panicked")?);
    }

    let mut ids: Vec<_> = filed.iter().map(|member| member.id()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), WRITERS, "no two writers minted the same id");

    let Outcome::Committed(heads) = conn
        .db()
        .run(&Query::scan(&Member::LATEST, all::<2>()))
        .await?
    else {
        panic!("a scan of the generated index answers");
    };
    assert_eq!(heads.len(), WRITERS, "every record is in the store");
    Ok(())
}

on_every_engine! {
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    one_writer_takes_the_key,
    no_update_is_lost,
    concurrent_inserts_all_land,
}
