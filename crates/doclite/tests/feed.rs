//! What a record that declares an owner is filed under, and what reading it
//! back answers.
//!
//! The index is what the feature is: an owner's changes are rows in one
//! ordered index, so what has to be true of them is what has to be true of
//! `deal_feed`.

use doclite::id::EPOCH_MS;
use doclite::{
    records, Checkpoint, Column, Connection, Document, DocumentError, FixedClock, NodeId, Record,
    Stored, Timestamp,
};
use doclite_core::{ColumnType, IndexId};
use serde::{Deserialize, Serialize};

mod under;
use under::{on_every_engine, Under};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Reference(String);

/// A deal owned by the account it belongs to, and indexed by it as well, which
/// is the pair `#[owner]` beside `#[index]` is for: one answers who it is
/// about now, the other what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Deal {
    #[unique]
    reference: Reference,

    #[owner]
    #[index]
    account: String,

    amount_cents: i64,
}

#[test]
fn an_owner_declares_a_feed_beside_the_record_s_other_indexes() {
    let named: Vec<IndexId> = Deal::schemas().iter().map(|schema| schema.id).collect();
    assert_eq!(
        named,
        [
            IndexId("deal_latest"),
            IndexId("deal_by_reference"),
            IndexId("deal_feed"),
            IndexId("deal_by_account"),
        ]
    );
}

/// The owner, then the revision, then the document. The revision in the middle
/// is what makes a scan of one owner read in the order its changes happened.
#[test]
fn a_feed_is_keyed_on_the_owner_the_revision_and_the_document() {
    let feed = Deal::schemas()
        .into_iter()
        .find(|schema| schema.id == IndexId("deal_feed"))
        .expect("the feed is declared");

    assert_eq!(
        feed.columns,
        [ColumnType::Str, ColumnType::I64, ColumnType::I64],
        "an id is declared as the signed integer it round-trips through"
    );
}

/// A record owned by several at once: the users watching a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Task {
    #[unique]
    title: String,

    #[owner]
    watchers: Vec<String>,

    done: bool,
}

/// A record that declares no owner, so nothing about it is ever announced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Note {
    #[unique]
    slug: String,

    body: String,
}

/// Whether a record has a feed is settled by the type, which is what lets a
/// write of one that has none publish nothing without reading anything.
#[test]
fn a_record_with_no_owner_declares_no_feed() {
    assert_eq!(Note::FEEDS, None);
    assert_eq!(Deal::FEEDS, Some(IndexId("deal_feed")));
    assert_eq!(Task::FEEDS, Some(IndexId("task_feed")));
}

/// Several owners do not widen the key: a column holds one owner, and a feed is
/// read for one owner whichever shape the field has. What changes is how many
/// rows one revision files, which is one per owner.
#[test]
fn a_feed_over_several_owners_is_keyed_on_one_of_them() {
    let feed = Task::schemas()
        .into_iter()
        .find(|schema| schema.id == IndexId("task_feed"))
        .expect("the feed is declared");

    assert_eq!(
        feed.columns,
        [ColumnType::Str, ColumnType::I64, ColumnType::I64],
        "the element answers for the column, not the list"
    );
}

fn clock() -> FixedClock {
    FixedClock(Timestamp(EPOCH_MS + 1_000))
}

async fn open<U: Under>() -> Connection<U::Engine, FixedClock> {
    Connection::with_clock(
        U::engine().await,
        records![Deal, Task],
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

fn amounts(changes: &[Stored<Deal>]) -> Vec<i64> {
    changes.iter().map(|deal| deal.amount_cents).collect()
}

fn task(title: &str, watchers: &[&str]) -> Task {
    Task {
        title: title.into(),
        watchers: watchers.iter().map(|who| (*who).to_string()).collect(),
        done: false,
    }
}

fn titles(changes: &[Stored<Task>]) -> Vec<String> {
    changes.iter().map(|task| task.title.clone()).collect()
}

/// Every revision, in the order it was written, each carrying what the record
/// held at the time rather than what it holds now.
async fn a_feed_holds_a_row_per_revision_oldest_first<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(deal("d-1", "roma", 100)).await?;
    filed.amount_cents = 200;
    let mut filed = conn.update(filed).await?;
    filed.amount_cents = 300;
    conn.update(filed).await?;

    let changes = Deal::feed(&conn, "roma".into()).get_many().await?;

    assert_eq!(amounts(&changes), [100, 200, 300]);
    assert!(
        changes
            .windows(2)
            .all(|pair| pair[0].revision() < pair[1].revision()),
        "a feed reads in the order the changes happened"
    );
    assert!(
        changes.iter().all(|change| change.id() == changes[0].id()),
        "one document, three revisions of it"
    );
    Ok(())
}

/// What a returning reader does. The checkpoint it kept is the last revision it
/// handled, and `after` is what makes resuming exact rather than one-past.
async fn a_feed_read_from_a_checkpoint_skips_what_came_before<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(deal("d-1", "roma", 100)).await?;
    filed.amount_cents = 200;
    let second = conn.update(filed).await?;
    let mut third = second.clone();
    third.amount_cents = 300;
    conn.update(third).await?;

    let since = Checkpoint::after(second.revision());
    let changes = Deal::feed(&conn, "roma".into())
        .since(since)
        .get_many()
        .await?;

    assert_eq!(amounts(&changes), [300]);

    let inclusive = Deal::feed(&conn, "roma".into())
        .since(Checkpoint::at(second.revision()))
        .get_many()
        .await?;
    assert_eq!(
        amounts(&inclusive),
        [200, 300],
        "`at` includes the revision it names"
    );
    Ok(())
}

/// A reader that kept only the time gets the whole millisecond back, because a
/// cursor built from one rounds down. Replaying is the safe direction; the
/// clock here is fixed, so all three revisions share that millisecond.
async fn a_feed_read_from_a_timestamp_replays_its_millisecond<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(deal("d-1", "roma", 100)).await?;
    filed.amount_cents = 200;
    conn.update(filed).await?;

    let changes = Deal::feed(&conn, "roma".into())
        .since(Timestamp(EPOCH_MS + 1_000))
        .get_many()
        .await?;

    assert_eq!(amounts(&changes), [100, 200]);

    let later = Deal::feed(&conn, "roma".into())
        .since(Timestamp(EPOCH_MS + 1_001))
        .get_many()
        .await?;
    assert!(later.is_empty(), "a later millisecond holds nothing yet");
    Ok(())
}

/// The owner is the first component of the key, so one owner's rows are a
/// group and a scan of it cannot wander into the next.
async fn a_feed_is_scoped_to_one_owner<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    conn.insert(deal("d-1", "roma", 100)).await?;
    conn.insert(deal("d-2", "sur", 200)).await?;
    conn.insert(deal("d-3", "roma", 300)).await?;

    assert_eq!(
        amounts(&Deal::feed(&conn, "roma".into()).get_many().await?),
        [100, 300]
    );
    assert_eq!(
        amounts(&Deal::feed(&conn, "sur".into()).get_many().await?),
        [200]
    );
    Ok(())
}

/// The whole difference between a feed and the indexes beside it. `#[index]`
/// holds what the record is now, one row; `#[owner]` holds what happened, one
/// row per edit.
async fn a_feed_row_outlives_the_retirement_of_every_other_row<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(deal("d-1", "roma", 100)).await?;
    filed.amount_cents = 200;
    conn.update(filed).await?;

    let current = Deal::query(&conn)
        .by_account(String::from("roma"))
        .get_many()
        .await?;
    assert_eq!(amounts(&current), [200], "one row, the live revision");

    let changes = Deal::feed(&conn, "roma".into()).get_many().await?;
    assert_eq!(amounts(&changes), [100, 200], "both revisions");
    Ok(())
}

/// The semantic this chose: a feed row is keyed on the owner the record held
/// when it was written, so a reassignment stops one feed and starts another.
/// The old owner is not told the record left, which is the cost.
async fn a_reassigned_record_stops_at_its_old_owner_s_last_revision<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(deal("d-1", "roma", 100)).await?;
    filed.account = "sur".into();
    let moved = conn.update(filed).await?;

    let left = Deal::feed(&conn, "roma".into()).get_many().await?;
    assert_eq!(amounts(&left), [100], "the revisions it was theirs for");
    assert!(
        left[0].revision() < moved.revision(),
        "and nothing after the move"
    );

    let arrived = Deal::feed(&conn, "sur".into()).get_many().await?;
    assert_eq!(amounts(&arrived), [100], "the revision that moved it");
    assert_eq!(arrived[0].revision(), moved.revision());
    Ok(())
}

/// Both ends are inclusive, so a window is written with the revisions it holds
/// rather than with the ones just outside it.
async fn a_feed_can_be_trimmed_at_the_high_end<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(deal("d-1", "roma", 100)).await?;
    filed.amount_cents = 200;
    let second = conn.update(filed).await?;
    let mut third = second.clone();
    third.amount_cents = 300;
    conn.update(third).await?;

    let changes = Deal::feed(&conn, "roma".into())
        .until(second.revision())
        .get_many()
        .await?;

    assert_eq!(amounts(&changes), [100, 200]);
    Ok(())
}

/// How a long feed is read: a page, then the next one from just past the last
/// revision the page held.
async fn a_feed_is_paged_by_its_own_last_revision<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(deal("d-1", "roma", 100)).await?;
    filed.amount_cents = 200;
    let mut filed = conn.update(filed).await?;
    filed.amount_cents = 300;
    conn.update(filed).await?;

    let page = Deal::feed(&conn, "roma".into()).limit(2).get_many().await?;
    assert_eq!(amounts(&page), [100, 200]);

    let last = page.last().expect("the page is not empty").revision();
    let rest = Deal::feed(&conn, "roma".into())
        .since(Checkpoint::after(last))
        .get_many()
        .await?;
    assert_eq!(amounts(&rest), [300]);
    Ok(())
}

/// One revision, a row in every owner's feed. The index does not change shape:
/// each row holds one owner, so each of them reads their own.
async fn a_record_with_several_owners_is_in_each_of_their_feeds<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(task("t-1", &["ana", "bruno"])).await?;

    assert_eq!(
        titles(&Task::feed(&conn, "ana".into()).get_many().await?),
        ["t-1"]
    );
    assert_eq!(
        titles(&Task::feed(&conn, "bruno".into()).get_many().await?),
        ["t-1"]
    );
    assert!(
        Task::feed(&conn, "carla".into())
            .get_many()
            .await?
            .is_empty(),
        "and nobody else's"
    );
    Ok(())
}

/// Dropping an owner is the reassignment semantic one owner at a time: their
/// feed keeps the revisions that were theirs and stops.
async fn an_owner_dropped_from_a_record_keeps_what_was_theirs<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let mut filed = conn.insert(task("t-1", &["ana", "bruno"])).await?;
    filed.watchers = vec!["ana".into()];
    let edited = conn.update(filed).await?;

    let ana = Task::feed(&conn, "ana".into()).get_many().await?;
    assert_eq!(ana.len(), 2, "both revisions were hers");
    assert_eq!(ana[1].revision(), edited.revision());

    let bruno = Task::feed(&conn, "bruno".into()).get_many().await?;
    assert_eq!(bruno.len(), 1, "only the revision he was on");
    assert!(bruno[0].revision() < edited.revision());
    Ok(())
}

/// A row is its key, so naming one owner twice files one row: the memory store
/// inserts into a set and the SQLite one ignores the second insert.
async fn a_repeated_owner_is_one_row<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(task("t-1", &["ana", "ana"])).await?;

    assert_eq!(
        titles(&Task::feed(&conn, "ana".into()).get_many().await?),
        ["t-1"],
        "one change rather than two of the same"
    );
    Ok(())
}

/// A record with no owners files no feed rows, so the revision is in nobody's
/// feed. The honest meaning of an empty list, and still a record the other
/// indexes find.
async fn a_record_with_no_owners_is_in_nobody_s_feed<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let filed = conn.insert(task("t-1", &[])).await?;

    assert!(Task::feed(&conn, "ana".into()).get_many().await?.is_empty());

    let found = Task::query(&conn)
        .by_title(String::from("t-1"))
        .get_one()
        .await?;
    assert_eq!(found.revision(), filed.revision(), "it is still filed");
    Ok(())
}

on_every_engine! {
    #[tokio::test]
    a_record_with_several_owners_is_in_each_of_their_feeds,
    an_owner_dropped_from_a_record_keeps_what_was_theirs,
    a_repeated_owner_is_one_row,
    a_record_with_no_owners_is_in_nobody_s_feed,
    a_feed_holds_a_row_per_revision_oldest_first,
    a_feed_read_from_a_checkpoint_skips_what_came_before,
    a_feed_read_from_a_timestamp_replays_its_millisecond,
    a_feed_is_scoped_to_one_owner,
    a_feed_row_outlives_the_retirement_of_every_other_row,
    a_reassigned_record_stops_at_its_old_owner_s_last_revision,
    a_feed_can_be_trimmed_at_the_high_end,
    a_feed_is_paged_by_its_own_last_revision,
}
