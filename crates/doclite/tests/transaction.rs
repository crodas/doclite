//! Several records filed as one operation, and what happens when one refuses.

use doclite::doclite_core::IndexSchema;
use doclite::{Column, Connection, Document, DocumentError, Record, Ref};
use serde::{Deserialize, Serialize};

mod under;
use under::{on_every_engine, Under};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Slug(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Column)]
enum Shelf {
    Front,
    Back,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Note {
    #[unique]
    slug: Slug,

    #[index]
    shelf: Shelf,

    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Mark {
    #[unique]
    tag: Slug,

    #[index]
    note: Ref<Note>,
}

fn schemas() -> Vec<IndexSchema> {
    let mut all = Note::schemas();
    all.extend(Mark::schemas());
    all
}

async fn store<U: Under>() -> Connection<U::Engine> {
    Connection::open(U::engine().await, &schemas())
        .await
        .expect("a fresh store")
}

fn note(slug: &str, shelf: Shelf) -> Note {
    Note {
        slug: Slug(slug.into()),
        shelf,
        body: String::new(),
    }
}

async fn notes<U: Under>(conn: &Connection<U::Engine>, shelf: Shelf) -> Vec<Note> {
    Note::query(conn)
        .by_shelf(shelf)
        .get_many()
        .await
        .expect("a scan of one index")
        .into_iter()
        .map(|stored| stored.into_inner())
        .collect()
}

async fn two_records_in_one_commit_both_land<U: Under>() -> Result<(), DocumentError> {
    let conn = store::<U>().await;

    let mut tx = conn.transaction();
    let filed = tx.insert(note("first", Shelf::Front)).expect("staged");
    tx.insert(Mark {
        tag: Slug("read".into()),
        note: filed.reference(),
    })
    .expect("staged");
    tx.commit().await.expect("one operation, both records");

    assert_eq!(notes::<U>(&conn, Shelf::Front).await.len(), 1);
    assert_eq!(
        Mark::query(&conn)
            .by_note(filed.reference())
            .get_many()
            .await
            .expect("a scan")
            .len(),
        1
    );

    Ok(())
}

/// The property the whole type exists for: a task written without the
/// transition that justifies it would be worse than neither.
async fn a_refusal_on_the_second_record_leaves_the_first_unwritten<U: Under>(
) -> Result<(), DocumentError> {
    let conn = store::<U>().await;
    conn.insert(note("taken", Shelf::Back))
        .await
        .expect("the key is claimed first");

    let mut tx = conn.transaction();
    tx.insert(note("fresh", Shelf::Front)).expect("staged");
    tx.insert(note("taken", Shelf::Front)).expect("staged");

    assert_eq!(
        tx.commit().await,
        Err(DocumentError::Duplicate {
            index: "note_by_slug"
        })
    );
    assert!(
        notes::<U>(&conn, Shelf::Front).await.is_empty(),
        "the record staged before the refusal is not in the store"
    );

    Ok(())
}

/// Two writes numbered from the same place would both answer to one code, so
/// this is the test that the numbering is per write rather than per layer.
async fn a_refusal_names_the_record_that_caused_it<U: Under>() -> Result<(), DocumentError> {
    let conn = store::<U>().await;
    conn.insert(Mark {
        tag: Slug("read".into()),
        note: conn
            .insert(note("anchor", Shelf::Back))
            .await
            .expect("an anchor")
            .reference(),
    })
    .await
    .expect("the tag is claimed first");

    let mut tx = conn.transaction();
    let filed = tx.insert(note("fresh", Shelf::Front)).expect("staged");
    tx.insert(Mark {
        tag: Slug("read".into()),
        note: filed.reference(),
    })
    .expect("staged");

    assert_eq!(
        tx.commit().await,
        Err(DocumentError::Duplicate {
            index: "mark_by_tag"
        }),
        "the second record's index, not the first record's"
    );

    Ok(())
}

async fn a_stale_edit_inside_a_commit_writes_nothing<U: Under>() -> Result<(), DocumentError> {
    let conn = store::<U>().await;
    let filed = conn
        .insert(note("moving", Shelf::Back))
        .await
        .expect("a record");

    let mut ahead = filed.clone();
    ahead.shelf = Shelf::Front;
    conn.update(ahead).await.expect("somebody else got there");

    let mut stale = filed;
    stale.body = "late".into();

    let mut tx = conn.transaction();
    tx.insert(note("alongside", Shelf::Front)).expect("staged");
    tx.update(stale).expect("staged");

    assert_eq!(tx.commit().await, Err(DocumentError::Stale));
    assert_eq!(
        notes::<U>(&conn, Shelf::Front).await.len(),
        1,
        "only the record the other writer moved"
    );

    Ok(())
}

async fn an_insert_and_an_edit_of_two_types_commit_together<U: Under>() -> Result<(), DocumentError>
{
    let conn = store::<U>().await;
    let anchor = conn
        .insert(note("anchor", Shelf::Back))
        .await
        .expect("a record");

    let mut moved = anchor.clone();
    moved.shelf = Shelf::Front;

    let mut tx = conn.transaction();
    tx.update(moved).expect("staged");
    tx.insert(Mark {
        tag: Slug("moved".into()),
        note: anchor.reference(),
    })
    .expect("staged");
    tx.commit().await.expect("both");

    assert_eq!(notes::<U>(&conn, Shelf::Front).await.len(), 1);
    assert!(notes::<U>(&conn, Shelf::Back).await.is_empty());

    Ok(())
}

/// A commit of one has to be the same write `Connection::insert` makes, or the
/// transaction is a second way of filing a record rather than the same one.
async fn one_record_committed_reads_back_as_one_record_inserted<U: Under>(
) -> Result<(), DocumentError> {
    let staged = store::<U>().await;
    let mut tx = staged.transaction();
    tx.insert(note("only", Shelf::Front)).expect("staged");
    tx.commit().await.expect("committed");

    let direct = store::<U>().await;
    direct
        .insert(note("only", Shelf::Front))
        .await
        .expect("inserted");

    assert_eq!(
        notes::<U>(&staged, Shelf::Front).await,
        notes::<U>(&direct, Shelf::Front).await
    );

    Ok(())
}

async fn a_transaction_that_is_dropped_writes_nothing<U: Under>() -> Result<(), DocumentError> {
    let conn = store::<U>().await;
    {
        let mut tx = conn.transaction();
        tx.insert(note("abandoned", Shelf::Front)).expect("staged");
    }

    assert!(notes::<U>(&conn, Shelf::Front).await.is_empty());

    Ok(())
}

async fn an_empty_commit_is_not_an_operation<U: Under>() -> Result<(), DocumentError> {
    let conn = store::<U>().await;
    let tx = conn.transaction();

    assert!(tx.is_empty());
    assert_eq!(tx.commit().await, Ok(()));

    Ok(())
}

on_every_engine! {
    #[tokio::test]
    two_records_in_one_commit_both_land,
    a_refusal_on_the_second_record_leaves_the_first_unwritten,
    a_refusal_names_the_record_that_caused_it,
    a_stale_edit_inside_a_commit_writes_nothing,
    an_insert_and_an_edit_of_two_types_commit_together,
    one_record_committed_reads_back_as_one_record_inserted,
    a_transaction_that_is_dropped_writes_nothing,
    an_empty_commit_is_not_an_operation,
}
