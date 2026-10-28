//! What a record that is never superseded is.
//!
//! `#[immutable]` says a record has exactly one revision, ever. That is a fact
//! about the schema rather than about anything the store holds, so it is
//! settled by the type: the derive leaves out the `Amendable` impl that
//! `Connection::update` asks for, and an edit does not compile. Nothing reaches
//! an engine, and there is no abort code for it.
//!
//! What is left is everything else a record does, and none of it changes. An
//! immutable record is filed under the same generated index, claims its unique
//! keys the same way, and is queried and followed like any other. These tests
//! are that half: the refusal itself is a compile error, checked by the
//! doctests on `doclite::Amendable`.

use doclite::doclite_core::{all, Index, Key, ObjectId, Outcome, Query};
use doclite::id::EPOCH_MS;
use doclite::{
    records, Column, Connection, Document, DocumentError, DocumentId, Filing, FixedClock, NodeId,
    Record, RevisionId, Rows, Stored, Timestamp,
};
use serde::{Deserialize, Serialize};

mod under;
use under::{on_every_engine, Under};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Slug(String);

/// One unique field and one plain one, so the walk yields both kinds and an
/// insert has a key to claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
#[immutable]
struct Entry {
    #[unique]
    slug: Slug,

    #[index]
    account: u8,

    amount: i64,
}

/// The same shape without the attribute, so a difference in what is filed can
/// only be the attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Note {
    #[unique]
    slug: Slug,

    #[index]
    account: u8,

    amount: i64,
}

/// Records what a record's walk yields, so a test can read it.
#[derive(Default)]
struct Walk(Vec<(&'static str, Filing)>);

impl Rows for Walk {
    fn row<const N: usize>(
        &mut self,
        _index: &Index<N>,
        _key: Key<N>,
        name: &'static str,
        filing: Filing,
    ) {
        self.0.push((name, filing));
    }
}

fn clock() -> FixedClock {
    FixedClock(Timestamp(EPOCH_MS + 1_000))
}

fn node() -> NodeId {
    NodeId::new(7).expect("seven fits in ten bits")
}

async fn open<U: Under>() -> Connection<U::Engine, FixedClock> {
    Connection::with_clock(U::engine().await, records![Entry], clock(), node())
        .await
        .expect("the engine accepts its own schemas")
}

fn entry(slug: &str, account: u8, amount: i64) -> Entry {
    Entry {
        slug: Slug(slug.into()),
        account,
        amount,
    }
}

async fn an_immutable_record_is_filed_and_found_like_any_other<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let filed = conn.insert(entry("e-1", 4, 100)).await?;

    let found = Entry::query(&conn)
        .by_slug(Slug("e-1".into()))
        .get_one()
        .await?;

    assert_eq!(found.id(), filed.id());
    assert_eq!(found.amount, 100);

    let by_account = Entry::query(&conn).by_account(4..=4).get_many().await?;
    assert_eq!(by_account.len(), 1, "the plain index answers too");

    Ok(())
}

async fn an_immutable_record_still_claims_its_unique_keys<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(entry("e-1", 4, 100)).await?;

    assert_eq!(
        conn.insert(entry("e-1", 9, 250)).await.err(),
        Some(DocumentError::Duplicate {
            index: "entry_by_slug"
        }),
        "immutability says nothing about who may hold a key"
    );

    Ok(())
}

async fn the_only_revision_of_an_immutable_record_is_the_first_one<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let filed = conn.insert(entry("e-1", 4, 100)).await?;

    assert_eq!(filed.prev(), None, "there is nothing behind it");
    assert_eq!(
        filed.id().0,
        filed.revision().0,
        "the first revision is the record"
    );
    assert!(
        conn.history(&filed).await?.is_empty(),
        "a chain of one is no chain"
    );

    Ok(())
}

async fn following_a_reference_to_an_immutable_record_finds_its_one_revision<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let filed = conn.insert(entry("e-1", 4, 100)).await?;

    let followed: Stored<Entry> = conn.load(filed.reference()).await?;

    assert_eq!(followed.revision(), filed.revision());
    assert_eq!(followed.amount, 100);

    Ok(())
}

async fn an_immutable_record_is_filed_under_the_generated_index_like_any_other<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let filed = conn.insert(entry("e-1", 4, 100)).await?;

    let mut walk = Walk::default();
    filed.rows(filed.id(), filed.revision(), &mut walk);

    assert_eq!(
        walk.0,
        [
            ("entry_latest", Filing::Latest),
            ("entry_by_slug", Filing::Unique),
            ("entry_by_account", Filing::Plain),
        ],
        "the same walk an amendable record does"
    );

    let Outcome::Committed(heads) = conn
        .db()
        .run(&Query::scan(&Entry::LATEST, all::<2>()))
        .await?
    else {
        panic!("a scan of the generated index answers");
    };
    assert_eq!(heads.len(), 1, "one row, as for any record");

    Ok(())
}

on_every_engine! {
    #[tokio::test]
    an_immutable_record_is_filed_and_found_like_any_other,
    an_immutable_record_still_claims_its_unique_keys,
    the_only_revision_of_an_immutable_record_is_the_first_one,
    following_a_reference_to_an_immutable_record_finds_its_one_revision,
    an_immutable_record_is_filed_under_the_generated_index_like_any_other,
}

/// The attribute changes which impls the derive writes and nothing a record
/// declares, so two structs differing only by it are filed the same way.
#[test]
fn immutability_changes_nothing_about_what_a_record_declares() {
    assert_eq!(Entry::schemas().len(), Note::schemas().len());

    let document = DocumentId(ObjectId(7));
    let revision = RevisionId(ObjectId(7));

    let mut immutable = Walk::default();
    entry("e-1", 4, 100).rows(document, revision, &mut immutable);

    let mut amendable = Walk::default();
    Note {
        slug: Slug("n-1".into()),
        account: 4,
        amount: 100,
    }
    .rows(document, revision, &mut amendable);

    let shape = |walk: &Walk| -> Vec<Filing> { walk.0.iter().map(|(_, filing)| *filing).collect() };
    assert_eq!(shape(&immutable), shape(&amendable));
}
