//! Being told about a change rather than reading it.
//!
//! A watch is the feed with a tail: the rows already there first, in order,
//! then the ones that have not happened yet. Nothing here is a second source of
//! truth, which is what the last scenario is about: a reader that falls behind
//! is sent back to the same index everything else reads.

use std::sync::Arc;

use doclite::id::EPOCH_MS;
use doclite::{
    records, Change, Changes, Checkpoint, Column, Connection, Document, DocumentError, FixedClock,
    NodeId, Stored, Timestamp,
};
use serde::{Deserialize, Serialize};

mod under;
use under::{on_every_engine, Under};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Reference(String);

/// A record with several owners at once, so one write reaches each of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Task {
    #[unique]
    title: String,

    #[owner]
    watchers: Vec<String>,
}

/// A record with no owner, which nothing can ever be told about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Note {
    #[unique]
    slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Deal {
    #[unique]
    reference: Reference,

    #[owner]
    account: String,

    amount_cents: i64,
}

fn clock() -> FixedClock {
    FixedClock(Timestamp(EPOCH_MS + 1_000))
}

async fn open<U: Under>() -> Connection<U::Engine, FixedClock> {
    Connection::with_clock(
        U::engine().await,
        records![Deal, Task, Note],
        clock(),
        NodeId::new(7).expect("seven fits in ten bits"),
    )
    .await
    .expect("the engine accepts its own schemas")
}

fn deal(reference: &str, account: &str, amount_cents: i64) -> Deal {
    Deal {
        reference: Reference(reference.into()),
        account: account.into(),
        amount_cents,
    }
}

/// The next change, insisting it is one rather than a lag.
async fn revised<E: doclite::doclite_core::Engine>(
    changes: &mut Changes<'_, E, FixedClock, Deal>,
) -> Result<Stored<Deal>, DocumentError> {
    match changes.next().await {
        Some(Ok(Change::Revised(deal))) => Ok(deal),
        Some(Ok(Change::Lagged { from })) => {
            panic!("this watcher is not behind, but it was told it lagged from {from:?}")
        }
        Some(Err(why)) => Err(why),
        None => panic!("the watch ended while the connection is still open"),
    }
}

/// What a reader that was away gets first: the rows the feed already held.
async fn a_watch_delivers_what_the_feed_already_held<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(deal("d-1", "roma", 100)).await?;
    filed.amount_cents = 200;
    conn.update(filed).await?;

    let mut changes = Deal::feed(&conn, "roma".into()).watch().await?;

    assert_eq!(revised(&mut changes).await?.amount_cents, 100);
    assert_eq!(revised(&mut changes).await?.amount_cents, 200);
    Ok(())
}

/// The live half. The notice is queued by the commit, so the reader finds it
/// waiting rather than racing for it.
async fn a_watch_delivers_a_change_made_after_it_started<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let mut changes = Deal::feed(&conn, "roma".into()).watch().await?;

    let filed = conn.insert(deal("d-1", "roma", 100)).await?;

    let heard = revised(&mut changes).await?;
    assert_eq!(heard.revision(), filed.revision());
    assert_eq!(heard.amount_cents, 100);
    Ok(())
}

/// A watcher reads only its own owner, for the same reason a feed does: the
/// subscription is a prefix of the key.
async fn a_watch_hears_nothing_about_another_owner<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let mut changes = Deal::feed(&conn, "roma".into()).watch().await?;

    conn.insert(deal("d-1", "sur", 100)).await?;
    let mine = conn.insert(deal("d-2", "roma", 200)).await?;

    let heard = revised(&mut changes).await?;
    assert_eq!(
        heard.revision(),
        mine.revision(),
        "the other owner's is not"
    );
    Ok(())
}

/// Resuming. The checkpoint the reader kept is where the catch-up starts, so
/// what it already handled is not handed to it again.
async fn a_watch_resumes_from_a_checkpoint<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(deal("d-1", "roma", 100)).await?;
    filed.amount_cents = 200;
    let second = conn.update(filed).await?;

    let mut changes = Deal::feed(&conn, "roma".into())
        .since(Checkpoint::after(second.revision()))
        .watch()
        .await?;

    let mut third = second.clone();
    third.amount_cents = 300;
    conn.update(third).await?;

    assert_eq!(revised(&mut changes).await?.amount_cents, 300);
    Ok(())
}

/// What a reader holds to come back with later. It tracks what was handed over
/// rather than what was read, so stopping in the middle of a catch-up and
/// resuming from it loses nothing.
async fn a_watcher_s_checkpoint_resumes_after_what_it_was_given<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let filed = conn.insert(deal("d-1", "roma", 100)).await?;

    let mut changes = Deal::feed(&conn, "roma".into()).watch().await?;
    assert_eq!(
        changes.checkpoint(),
        Checkpoint::origin(),
        "the caught-up row has not been handed over yet"
    );

    revised(&mut changes).await?;
    assert_eq!(changes.checkpoint(), Checkpoint::after(filed.revision()));

    let next = conn.insert(deal("d-2", "roma", 200)).await?;
    revised(&mut changes).await?;
    assert_eq!(changes.checkpoint(), Checkpoint::after(next.revision()));

    let rest = Deal::feed(&conn, "roma".into())
        .since(changes.checkpoint())
        .get_many()
        .await?;
    assert!(rest.is_empty(), "there is nothing after it yet");
    Ok(())
}

/// The point of the durable half. A reader that stops draining loses notices,
/// is told so once, and is handed the rows it missed out of the feed.
async fn a_watcher_that_falls_behind_is_told_and_catches_up<U: Under>() -> Result<(), DocumentError>
{
    let conn = open::<U>().await;
    let mut changes = Deal::feed(&conn, "roma".into()).backlog(1).watch().await?;

    let mut filed = conn.insert(deal("d-1", "roma", 0)).await?;
    for amount in 1..=5 {
        filed.amount_cents = amount * 100;
        filed = conn.update(filed).await?;
    }

    let Some(Ok(Change::Lagged { from })) = changes.next().await else {
        panic!("a watcher given six changes and a backlog of one is behind");
    };
    assert_eq!(from, Checkpoint::origin(), "nothing had been delivered yet");

    let mut heard = Vec::new();
    for _ in 0..6 {
        heard.push(revised(&mut changes).await?.amount_cents);
    }
    assert_eq!(
        heard,
        [0, 100, 200, 300, 400, 500],
        "every change, in order, read out of the feed"
    );
    Ok(())
}

/// The seam between the two halves, under a writer that does not wait for it.
/// A change may be both scanned and announced, and a reader must see it once.
async fn every_change_reaches_a_watcher_once_while_a_writer_races_it<U: Under>(
) -> Result<(), DocumentError>
where
    U::Engine: Send + Sync,
{
    let conn = Arc::new(open::<U>().await);
    let writing = Arc::clone(&conn);

    let writer = tokio::spawn(async move {
        let mut filed = writing.insert(deal("d-1", "roma", 0)).await?;
        for amount in 1..=19 {
            filed.amount_cents = amount * 100;
            filed = writing.update(filed).await?;
        }
        Ok::<(), DocumentError>(())
    });

    let mut changes = Deal::feed(&conn, "roma".into()).watch().await?;

    let mut heard = Vec::new();
    while heard.len() < 20 {
        match changes.next().await {
            Some(Ok(Change::Revised(deal))) => heard.push(deal.revision()),
            Some(Ok(Change::Lagged { .. })) => continue,
            Some(Err(why)) => return Err(why),
            None => panic!("the watch ended while the connection is still open"),
        }
    }

    writer
        .await
        .expect("the writing task did not panic")
        .expect("every write was filed");

    let mut once = heard.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!(once.len(), heard.len(), "no change was delivered twice");

    let filed = Deal::feed(&conn, "roma".into()).get_many().await?;
    assert_eq!(
        once,
        filed
            .iter()
            .map(Stored::revision)
            .take(20)
            .collect::<Vec<_>>(),
        "and none was missed"
    );
    Ok(())
}

/// One write, every owner told. Each of them is watching their own feed, and
/// the revision filed a row in each.
async fn every_owner_of_one_record_is_told<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut ana = Task::feed(&conn, "ana".into()).watch().await?;
    let mut bruno = Task::feed(&conn, "bruno".into()).watch().await?;

    let filed = conn
        .insert(Task {
            title: "t-1".into(),
            watchers: vec!["ana".into(), "bruno".into()],
        })
        .await?;

    for changes in [&mut ana, &mut bruno] {
        let Some(Ok(Change::Revised(heard))) = changes.next().await else {
            panic!("every owner of a record hears about it");
        };
        assert_eq!(heard.revision(), filed.revision());
    }
    Ok(())
}

/// A record with no owner cannot be announced, so a watcher never hears of one.
/// Proved by writing one first: the change it finds is the later write.
async fn a_watcher_hears_nothing_about_a_record_with_no_owner<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let mut changes = Deal::feed(&conn, "roma".into()).watch().await?;

    conn.insert(Note { slug: "n-1".into() }).await?;
    let mine = conn.insert(deal("d-1", "roma", 100)).await?;

    assert_eq!(revised(&mut changes).await?.revision(), mine.revision());
    Ok(())
}

on_every_engine! {
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    every_owner_of_one_record_is_told,
    a_watcher_hears_nothing_about_a_record_with_no_owner,
    a_watch_delivers_what_the_feed_already_held,
    a_watch_delivers_a_change_made_after_it_started,
    a_watch_hears_nothing_about_another_owner,
    a_watch_resumes_from_a_checkpoint,
    a_watcher_s_checkpoint_resumes_after_what_it_was_given,
    a_watcher_that_falls_behind_is_told_and_catches_up,
    every_change_reaches_a_watcher_once_while_a_writer_races_it,
}
