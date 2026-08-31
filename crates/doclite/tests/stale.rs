//! What happens when an update does not happen.
//!
//! A record is a chain of revisions and only the last one can be written. A
//! caller holding an earlier one is not editing an old copy of the record, it
//! is proposing a second head: a revision whose `prev` names something already
//! superseded, with whatever came after it discarded. The generated
//! `<record>_latest` index is what turns that into a refusal.
//!
//! Refusal is not the only way an update fails, and the ways are not
//! interchangeable. A stale revision and a taken unique key are both aborts and
//! can both be true at once. A conflict is not an abort at all: the engine says
//! it lost a race, the executor walks the operation again, and nobody finds out
//! unless the attempts run out.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use doclite::doclite_core::{
    all, Answers, Applied, Batch, Engine, IndexSchema, Query, Reads, StorageError,
};
use doclite::id::EPOCH_MS;
use doclite::{
    Column, Connection, Document, DocumentError, FixedClock, NodeId, Record, Stored, Timestamp,
};
use serde::{Deserialize, Serialize};

mod under;
use under::{on_every_engine, Under};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Slug(String);

/// One field of each kind, so an edit can miss every index the struct declared
/// and still have to answer for which revision it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Note {
    #[unique]
    slug: Slug,

    #[index]
    shelf: u8,

    body: String,
}

fn clock() -> FixedClock {
    FixedClock(Timestamp(EPOCH_MS + 1_000))
}

fn node() -> NodeId {
    NodeId::new(7).expect("seven fits in ten bits")
}

async fn open<U: Under>() -> Connection<U::Engine, FixedClock> {
    Connection::with_clock(U::engine().await, &Note::schemas(), clock(), node())
        .await
        .expect("the engine accepts its own schemas")
}

fn note(slug: &str, shelf: u8, body: &str) -> Note {
    Note {
        slug: Slug(slug.into()),
        shelf,
        body: body.into(),
    }
}

async fn head<U: Under>(
    conn: &Connection<U::Engine, FixedClock>,
) -> Result<Stored<Note>, DocumentError> {
    Note::query(conn)
        .by_slug(Slug("n-1".into()))
        .get_one()
        .await
}

/// Files a note and edits it twice, so the store holds 1 -> 2 -> 3 and 3 is the
/// head. Handed back oldest first: only the last of them can still be written.
async fn chain<U: Under>(
    conn: &Connection<U::Engine, FixedClock>,
) -> Result<[Stored<Note>; 3], DocumentError> {
    let first = conn.insert(note("n-1", 1, "one")).await?;

    let mut second = head::<U>(conn).await?;
    second.shelf = 2;
    let second = conn.update(second).await?;

    let mut third = head::<U>(conn).await?;
    third.shelf = 3;
    let third = conn.update(third).await?;

    Ok([first, second, third])
}

async fn editing_the_head_of_the_chain_supersedes_it<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let [first, second, third] = chain::<U>(&conn).await?;

    let mut editing = third.clone();
    editing.shelf = 4;
    let fourth = conn.update(editing).await?;

    assert_eq!(fourth.prev(), Some(third.revision()));
    assert_eq!(fourth.id(), first.id(), "still the same note");
    assert_eq!(head::<U>(&conn).await?.shelf, 4);

    let behind: Vec<u8> = conn
        .history(&fourth)
        .await?
        .iter()
        .map(|note| note.shelf)
        .collect();
    assert_eq!(behind, [3, 2, 1]);
    assert_eq!(second.shelf, 2, "the handles still read as what they were");
    Ok(())
}

/// The case this whole file is about: 1 -> 2 -> 3 with 3 live, and a caller
/// holding 2 asks to save it.
async fn editing_a_revision_from_the_middle_of_the_chain_is_refused<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let [_, second, _] = chain::<U>(&conn).await?;

    let mut editing = second;
    editing.shelf = 9;
    assert_eq!(conn.update(editing).await, Err(DocumentError::Stale));
    Ok(())
}

async fn editing_the_first_revision_of_the_chain_is_refused<U: Under>() -> Result<(), DocumentError>
{
    let conn = open::<U>().await;
    let [first, _, _] = chain::<U>(&conn).await?;

    let mut editing = first;
    editing.shelf = 9;
    assert_eq!(conn.update(editing).await, Err(DocumentError::Stale));
    Ok(())
}

/// Staleness is about position in the chain, not about age. The head is
/// writable right up until it is not.
async fn a_revision_stops_being_editable_once_it_is_superseded<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let [_, _, third] = chain::<U>(&conn).await?;

    let mut once = third.clone();
    once.shelf = 4;
    conn.update(once).await?;

    let mut again = third;
    again.shelf = 5;
    assert_eq!(conn.update(again).await, Err(DocumentError::Stale));
    assert_eq!(head::<U>(&conn).await?.shelf, 4);
    Ok(())
}

/// The refusal is worth having only because of what it prevents: two revisions
/// both claiming to be current, which is what a second row in the generated
/// index would be.
///
/// There is no `by_` method for that index, because it is keyed on ids rather
/// than on fields, so this reads it the way the operation does.
async fn a_refused_edit_leaves_the_chain_unbranched<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let [_, second, third] = chain::<U>(&conn).await?;

    let mut editing = second;
    editing.shelf = 9;
    assert_eq!(conn.update(editing).await, Err(DocumentError::Stale));

    let live = conn
        .db()
        .run(&Query::scan(&Note::LATEST, all::<2>()))
        .await?
        .committed()
        .expect("a scan of an index this connection declared");

    assert_eq!(live.len(), 1, "one record, one head");
    assert_eq!(live[0].object, third.revision().0, "and the head is 3");
    Ok(())
}

/// A fork would have lost whatever revision 3 changed. Nothing did.
async fn the_edit_that_was_refused_is_nowhere_in_the_record<U: Under>() -> Result<(), DocumentError>
{
    let conn = open::<U>().await;
    let [_, second, _] = chain::<U>(&conn).await?;

    let mut editing = second;
    editing.shelf = 9;
    editing.body = "a lost update".into();
    assert_eq!(conn.update(editing).await, Err(DocumentError::Stale));

    let live = head::<U>(&conn).await?;
    assert_eq!(live.shelf, 3, "revision 3 survived");
    assert_eq!(live.body, "one");

    let shelves: Vec<u8> = conn
        .history(&live)
        .await?
        .iter()
        .map(|note| note.shelf)
        .collect();
    assert_eq!(shelves, [2, 1], "and gained nothing behind it");

    assert!(Note::query(&conn).by_shelf(9).get_many().await?.is_empty());
    Ok(())
}

/// Reading the past is fine. It is only writing it that is refused.
async fn history_reads_from_a_revision_that_cannot_be_written<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let [_, second, _] = chain::<U>(&conn).await?;

    let behind: Vec<u8> = conn
        .history(&second)
        .await?
        .iter()
        .map(|note| note.shelf)
        .collect();
    assert_eq!(behind, [1], "everything the middle revision superseded");
    Ok(())
}

/// What a caller is meant to do about a refusal, written out once.
async fn an_edit_can_be_retried_against_what_superseded_it<U: Under>() -> Result<(), DocumentError>
{
    let conn = open::<U>().await;
    let [_, second, _] = chain::<U>(&conn).await?;

    let mut editing = second;
    editing.body = "a better body".into();
    assert_eq!(conn.update(editing).await, Err(DocumentError::Stale));

    let mut again = head::<U>(&conn).await?;
    again.body = "a better body".into();
    let saved = conn.update(again).await?;

    assert_eq!(saved.body, "a better body", "the edit landed");
    assert_eq!(saved.shelf, 3, "on top of what superseded it");
    Ok(())
}

/// An unedited handle takes the other branch in `Connection::update`, where the
/// value it was read as is the value it still holds. It is judged the same way.
async fn an_edit_that_changed_nothing_is_still_refused<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let [_, second, _] = chain::<U>(&conn).await?;

    assert_eq!(conn.update(second).await, Err(DocumentError::Stale));
    Ok(())
}

/// `body` is in no index, so this edit retires and files exactly the rows a
/// no-op would. The question was never about what moved.
async fn an_edit_of_a_field_no_index_mentions_is_still_refused<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let [_, second, _] = chain::<U>(&conn).await?;

    let mut editing = second;
    editing.body = "only the body".into();
    assert_eq!(conn.update(editing).await, Err(DocumentError::Stale));
    assert_eq!(head::<U>(&conn).await?.body, "one");
    Ok(())
}

/// Both preconditions would refuse this, and the caller is told about the one
/// that makes the other moot.
///
/// Which comes first is forced rather than chosen. `require_current` has to be
/// asserted before the old rows are retired, or read-your-own-writes shows it
/// the delete of the very row it is counting and every update is stale.
/// `require_unique` has to come after, or a record editing some other field
/// finds its own key and calls it a duplicate. Those two put staleness first
/// with nothing left to decide.
async fn staleness_is_reported_before_a_taken_unique_key<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let [_, second, _] = chain::<U>(&conn).await?;
    conn.insert(note("n-2", 7, "another")).await?;

    let mut editing = second;
    editing.slug = Slug("n-2".into());
    assert_eq!(conn.update(editing).await, Err(DocumentError::Stale));
    Ok(())
}

/// A record whose only index is unique.
///
/// The generated index is what makes this record editable at all, and what
/// makes a stale edit of it a refusal. Asserting on `by_ticket` could not: the
/// key carries no id, so a concurrent supersede that left the ticket alone
/// would leave one row there either way and the count would still be one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Ticket {
    #[unique]
    ticket: String,
    note: String,
}

async fn a_stale_edit_of_a_unique_only_record_loses_rather_than_clobbering<U: Under>(
) -> Result<(), DocumentError> {
    let conn =
        Connection::with_clock(U::engine().await, &Ticket::schemas(), clock(), node()).await?;

    conn.insert(Ticket {
        ticket: "T-1".into(),
        note: "first".into(),
    })
    .await?;

    let find = || Ticket::query(&conn).by_ticket("T-1".to_string()).get_one();
    let mut mine = find().await?;
    let mut theirs = find().await?;

    theirs.note = "theirs".into();
    conn.update(theirs).await?;

    mine.note = "mine".into();
    assert_eq!(conn.update(mine).await, Err(DocumentError::Stale));

    assert_eq!(find().await?.note, "theirs");
    Ok(())
}

/// An engine that loses a race on purpose.
///
/// A conflict cannot be staged through an engine that never loses one,
/// and it is the failure that is *not* a refusal: the executor is expected to
/// swallow it and walk the operation again.
struct Balk<E> {
    inner: E,
    refuse: AtomicUsize,
}

impl<E> Balk<E> {
    fn new(inner: E) -> Self {
        Self {
            inner,
            refuse: AtomicUsize::new(0),
        }
    }

    /// Report a conflict for the next `times` batches, whatever they hold.
    fn balk(&self, times: usize) {
        self.refuse.store(times, Ordering::Relaxed);
    }

    /// How many refusals are still owed, so a test can tell a retry from a
    /// batch that never reached the engine.
    fn owed(&self) -> usize {
        self.refuse.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl<E: Engine> Engine for Balk<E> {
    async fn define(&self, indexes: &[IndexSchema]) -> Result<(), StorageError> {
        self.inner.define(indexes).await
    }

    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        self.inner.read(reads).await
    }

    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError> {
        if self.refuse.load(Ordering::Relaxed) > 0 {
            self.refuse.fetch_sub(1, Ordering::Relaxed);
            return Ok(Applied::Conflict);
        }
        self.inner.apply(batch).await
    }
}

async fn balking<U: Under>() -> Connection<Balk<U::Engine>, FixedClock> {
    Connection::with_clock(
        Balk::new(U::engine().await),
        &Note::schemas(),
        clock(),
        node(),
    )
    .await
    .expect("the engine accepts its own schemas")
}

/// A conflict is not an abort. The caller asked for an edit and got one.
async fn a_conflict_is_retried_rather_than_refused<U: Under>() -> Result<(), DocumentError> {
    let conn = balking::<U>().await;
    conn.insert(note("n-1", 1, "one")).await?;

    conn.db().engine().balk(1);

    let mut editing = Note::query(&conn)
        .by_slug(Slug("n-1".into()))
        .get_one()
        .await?;
    editing.shelf = 2;
    let saved = conn.update(editing).await?;

    assert_eq!(saved.shelf, 2);
    assert_eq!(conn.db().engine().owed(), 0, "the conflict was spent");
    Ok(())
}

/// Until it runs out of attempts, and then it is an error rather than an
/// answer: nothing about the operation was wrong.
async fn a_conflict_that_never_clears_is_a_storage_error<U: Under>() -> Result<(), DocumentError> {
    let conn = balking::<U>().await;
    conn.insert(note("n-1", 1, "one")).await?;

    conn.db().engine().balk(usize::MAX);

    let mut editing = Note::query(&conn)
        .by_slug(Slug("n-1".into()))
        .get_one()
        .await?;
    editing.shelf = 2;

    assert!(
        matches!(
            conn.update(editing).await,
            Err(DocumentError::Storage(StorageError::Conflict { .. }))
        ),
        "an exhausted retry is not an abort"
    );
    Ok(())
}

on_every_engine! {
    #[tokio::test]
    editing_the_head_of_the_chain_supersedes_it,
    editing_a_revision_from_the_middle_of_the_chain_is_refused,
    editing_the_first_revision_of_the_chain_is_refused,
    a_revision_stops_being_editable_once_it_is_superseded,
    a_refused_edit_leaves_the_chain_unbranched,
    the_edit_that_was_refused_is_nowhere_in_the_record,
    history_reads_from_a_revision_that_cannot_be_written,
    an_edit_can_be_retried_against_what_superseded_it,
    an_edit_that_changed_nothing_is_still_refused,
    an_edit_of_a_field_no_index_mentions_is_still_refused,
    staleness_is_reported_before_a_taken_unique_key,
    a_stale_edit_of_a_unique_only_record_loses_rather_than_clobbering,
    a_conflict_is_retried_rather_than_refused,
    a_conflict_that_never_clears_is_a_storage_error,
}
