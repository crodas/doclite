//! A small CRM, as a worked example of what the derive is for.
//!
//! Two records that reference each other, a pipeline to query, and a deal that
//! moves through stages. The revision history a CRM would otherwise have to
//! build on purpose is what `prev` already is.

use doclite::id::EPOCH_MS;
use doclite::{
    greater_than, Column, Connection, Document, DocumentError, DocumentId, FixedClock, NodeId,
    Record, Ref, Stored, Timestamp,
};
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use doclite_core::{
    Answers, Applied, Batch, Engine, IndexSchema, ObjectId, Reads, StorageError, ToOrdBytes,
};
use serde::{Deserialize, Serialize};

mod under;
use under::{on_every_engine, Under};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct TaxId(String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Reference(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Column)]
enum Country {
    Argentina,
    Brazil,
    Paraguay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Column)]
enum Rep {
    Ana,
    Bruno,
    Carla,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Column)]
enum Stage {
    Lead,
    Qualified,
    Proposal,
    Won,
    Lost,
}

/// A company we sell to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
#[index(country, city)]
struct Account {
    #[unique]
    tax_id: TaxId,

    name: String,

    country: Country,

    city: String,

    #[index]
    owner: Rep,
}

/// A sale in progress, against an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
#[index(owner, stage)]
struct Deal {
    #[unique]
    reference: Reference,

    /// The account this is against. A `Ref` is an index column like any other,
    /// so the join is a scan of one index, and it names the account rather than
    /// one revision of it.
    #[index]
    account: Ref<Account>,

    owner: Rep,

    stage: Stage,

    /// Money as an integer of the smallest unit. Signed, so a credit note sorts
    /// below zero rather than wrapping past every sale.
    #[index]
    amount_cents: i64,

    closes_on: Option<String>,
}

fn schemas() -> Vec<IndexSchema> {
    let mut all = Account::schemas();
    all.extend(Deal::schemas());
    all
}

async fn open<U: Under>() -> Connection<U::Engine, FixedClock> {
    Connection::with_clock(
        U::engine().await,
        &schemas(),
        FixedClock(Timestamp(EPOCH_MS + 1_000)),
        NodeId::new(1).expect("one fits in ten bits"),
    )
    .await
    .expect("the engine accepts its own schemas")
}

fn account(tax_id: &str, name: &str, country: Country, city: &str, owner: Rep) -> Account {
    Account {
        tax_id: TaxId(tax_id.into()),
        name: name.into(),
        country,
        city: city.into(),
        owner,
    }
}

fn deal(reference: &str, account: Ref<Account>, owner: Rep, stage: Stage, cents: i64) -> Deal {
    Deal {
        reference: Reference(reference.into()),
        account,
        owner,
        stage,
        amount_cents: cents,
        closes_on: None,
    }
}

/// Two accounts, four deals, which is the shape every query below reads from.
async fn seeded<U: Under>(
) -> Result<(Connection<U::Engine, FixedClock>, Stored<Account>), DocumentError> {
    let conn = open::<U>().await;

    let roma = conn
        .insert(account(
            "80012345-6",
            "Clinica Roma",
            Country::Paraguay,
            "Asuncion",
            Rep::Ana,
        ))
        .await?;
    let sur = conn
        .insert(account(
            "80098765-4",
            "Distribuidora Sur",
            Country::Paraguay,
            "Encarnacion",
            Rep::Bruno,
        ))
        .await?;

    conn.insert(deal(
        "DEAL-2026-0001",
        roma.reference(),
        Rep::Ana,
        Stage::Proposal,
        4_500_000,
    ))
    .await?;
    conn.insert(deal(
        "DEAL-2026-0002",
        roma.reference(),
        Rep::Ana,
        Stage::Qualified,
        900_000,
    ))
    .await?;
    conn.insert(deal(
        "DEAL-2026-0003",
        sur.reference(),
        Rep::Bruno,
        Stage::Proposal,
        12_000_000,
    ))
    .await?;
    conn.insert(deal(
        "DEAL-2026-0004",
        sur.reference(),
        Rep::Bruno,
        Stage::Won,
        2_000_000,
    ))
    .await?;

    Ok((conn, roma))
}

async fn the_deals_on_one_account<U: Under>() -> Result<(), DocumentError> {
    let (conn, roma) = seeded::<U>().await?;

    let theirs = Deal::query(&conn)
        .by_account(roma.reference())
        .get_many()
        .await?;
    let references: Vec<&str> = theirs
        .iter()
        .map(|deal| deal.reference.0.as_str())
        .collect();
    assert_eq!(references, ["DEAL-2026-0001", "DEAL-2026-0002"]);
    Ok(())
}

async fn one_reps_pipeline_at_one_stage<U: Under>() -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;

    let proposals = Deal::query(&conn)
        .by_owner_stage(Rep::Ana, Stage::Proposal)
        .get_many()
        .await?;
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].amount_cents, 4_500_000);

    let everything_of_anas = Deal::query(&conn).by_owner(Rep::Ana).get_many().await?;
    assert_eq!(everything_of_anas.len(), 2);
    Ok(())
}

/// The whole point of a document id being separate from a revision id.
///
/// A deal names its account by the account, not by the revision it was read
/// from, so editing the account does not orphan every deal against it.
async fn a_foreign_key_survives_an_edit_of_what_it_points_at<U: Under>() -> Result<(), DocumentError>
{
    let (conn, roma) = seeded::<U>().await?;

    let mut account = Account::query(&conn)
        .by_tax_id(TaxId("80012345-6".into()))
        .get_one()
        .await?;
    account.city = "Luque".into();
    let moved = conn.update(account).await?;

    assert_eq!(moved.id(), roma.id(), "the same account, a later revision");
    assert_ne!(moved.revision(), roma.revision());

    let theirs = Deal::query(&conn)
        .by_account(roma.reference())
        .get_many()
        .await?;
    assert_eq!(theirs.len(), 2, "the deals still find their account");
    Ok(())
}

async fn deals_above_a_threshold<U: Under>() -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;

    let large = Deal::query(&conn)
        .by_amount_cents(2_000_000..)
        .get_many()
        .await?;
    let mut amounts: Vec<i64> = large.iter().map(|deal| deal.amount_cents).collect();
    amounts.sort_unstable();
    assert_eq!(amounts, [2_000_000, 4_500_000, 12_000_000]);

    let strictly_above = Deal::query(&conn)
        .by_amount_cents(greater_than(2_000_000i64))
        .get_many()
        .await?;
    assert_eq!(strictly_above.len(), 2);

    let band = Deal::query(&conn)
        .by_amount_cents(1_000_000..5_000_000)
        .get_many()
        .await?;
    assert_eq!(band.len(), 2);
    Ok(())
}

/// A credit note is a negative amount, and it has to sort below every sale
/// rather than above them, which is what the sign-bit flip in the encoding buys.
async fn a_credit_note_sorts_below_zero<U: Under>() -> Result<(), DocumentError> {
    let (conn, roma) = seeded::<U>().await?;
    conn.insert(deal(
        "DEAL-2026-0005",
        roma.reference(),
        Rep::Ana,
        Stage::Won,
        -750_000,
    ))
    .await?;

    let refunds = Deal::query(&conn).by_amount_cents(..0).get_many().await?;
    assert_eq!(refunds.len(), 1);
    assert_eq!(refunds[0].amount_cents, -750_000);

    let sales = Deal::query(&conn).by_amount_cents(0..).get_many().await?;
    assert_eq!(sales.len(), 4);
    Ok(())
}

/// The deal holds the account, so getting there is following it rather than
/// looking the account up by something else and checking the ids agree.
async fn the_account_behind_a_deal<U: Under>() -> Result<(), DocumentError> {
    let (conn, roma) = seeded::<U>().await?;

    let found = Deal::query(&conn)
        .by_reference(Reference("DEAL-2026-0001".into()))
        .get_one()
        .await?;

    let their_account = conn.load(found.account).await?;

    assert_eq!(their_account.id(), roma.id());
    assert_eq!(their_account.name, "Clinica Roma");
    assert_eq!(their_account.tax_id, TaxId("80012345-6".into()));
    Ok(())
}

/// A reference names the record, so following one after the record has been
/// edited answers with the edit rather than with what was there when the
/// reference was written.
async fn following_a_reference_answers_the_live_revision<U: Under>() -> Result<(), DocumentError> {
    let (conn, roma) = seeded::<U>().await?;

    let mut moving = conn.load(roma.reference()).await?;
    moving.city = "Luque".into();
    conn.update(moving).await?;

    let now = conn.load(roma.reference()).await?;

    assert_eq!(now.city, "Luque");
    assert_eq!(now.id(), roma.id(), "the same account");
    assert_ne!(now.revision(), roma.revision(), "a later revision");
    Ok(())
}

/// One scan rather than one read per deal, which is the reason `load_many`
/// exists at all.
async fn following_every_deals_account_at_once<U: Under>() -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;

    let deals = Deal::query(&conn).by_owner(Rep::Ana).get_many().await?;
    let accounts = conn
        .load_many(deals.iter().map(|deal| deal.account))
        .await?;

    let names: Vec<&str> = accounts.iter().map(|held| held.name.as_str()).collect();
    assert_eq!(
        names,
        ["Clinica Roma"],
        "both of Ana's deals name one account, and it comes back once"
    );
    Ok(())
}

async fn following_no_references_reads_nothing<U: Under>() -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;

    let none: Vec<Ref<Account>> = Vec::new();
    assert!(conn.load_many(none).await?.is_empty());
    Ok(())
}

async fn a_reference_to_a_record_that_is_not_there_is_not_found<U: Under>(
) -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;
    let nobody: Ref<Account> = Ref::new(DocumentId(ObjectId(1)));

    assert_eq!(conn.load(nobody).await, Err(DocumentError::NotFound));
    assert!(conn.load_many([nobody]).await?.is_empty());
    Ok(())
}

async fn accounts_in_one_city<U: Under>() -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;

    assert_eq!(
        Account::query(&conn)
            .by_country(Country::Paraguay)
            .get_many()
            .await?
            .len(),
        2
    );
    let asuncion = Account::query(&conn)
        .by_country_city(Country::Paraguay, "Asuncion".to_string())
        .get_many()
        .await?;
    assert_eq!(asuncion.len(), 1);
    assert_eq!(asuncion[0].name, "Clinica Roma");
    Ok(())
}

async fn advancing_a_deal_files_a_new_revision<U: Under>() -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;

    let mut deal = Deal::query(&conn)
        .by_reference(Reference("DEAL-2026-0001".into()))
        .get_one()
        .await?;

    let before = deal.revision();
    deal.stage = Stage::Won;
    let after = conn.update(deal).await?;

    assert_eq!(after.stage, Stage::Won);
    assert_eq!(after.prev(), Some(before));

    assert!(Deal::query(&conn)
        .by_owner_stage(Rep::Ana, Stage::Proposal)
        .get_many()
        .await?
        .is_empty());
    assert_eq!(
        Deal::query(&conn)
            .by_owner_stage(Rep::Ana, Stage::Won)
            .get_many()
            .await?
            .len(),
        1
    );
    Ok(())
}

/// The audit trail a CRM would otherwise build on purpose.
async fn a_deals_history_is_every_stage_it_passed_through<U: Under>() -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;

    let find = || {
        Deal::query(&conn)
            .by_reference(Reference("DEAL-2026-0002".into()))
            .get_one()
    };

    let mut advancing = find().await?;
    advancing.stage = Stage::Proposal;
    conn.update(advancing).await?;

    let mut closing = find().await?;
    closing.stage = Stage::Won;
    closing.amount_cents = 1_100_000;
    let latest = conn.update(closing).await?;

    let history = conn.history(&latest).await?;

    let stages: Vec<Stage> = history.iter().map(|deal| deal.stage).collect();
    assert_eq!(stages, [Stage::Proposal, Stage::Qualified]);

    let amounts: Vec<i64> = history.iter().map(|deal| deal.amount_cents).collect();
    assert_eq!(
        amounts,
        [900_000, 900_000],
        "the price only moved on the last edit"
    );

    assert_eq!(latest.amount_cents, 1_100_000);
    assert!(
        history[0].created_at() <= latest.created_at(),
        "a revision is never older than what it superseded"
    );
    Ok(())
}

async fn a_reference_is_taken_once<U: Under>() -> Result<(), DocumentError> {
    let (conn, roma) = seeded::<U>().await?;

    assert_eq!(
        conn.insert(deal(
            "DEAL-2026-0001",
            roma.reference(),
            Rep::Carla,
            Stage::Lead,
            1
        ))
        .await,
        Err(DocumentError::Duplicate {
            index: "deal_by_reference"
        })
    );
    assert_eq!(
        Deal::query(&conn)
            .by_owner(Rep::Carla)
            .get_many()
            .await?
            .len(),
        0
    );
    Ok(())
}

async fn two_reps_editing_one_deal<U: Under>() -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;

    let find = || {
        Deal::query(&conn)
            .by_reference(Reference("DEAL-2026-0003".into()))
            .get_one()
    };

    let mut ana = find().await?;
    let mut bruno = find().await?;

    bruno.stage = Stage::Won;
    conn.update(bruno).await?;

    ana.stage = Stage::Lost;
    assert_eq!(conn.update(ana).await, Err(DocumentError::Stale));

    let live = Deal::query(&conn)
        .by_reference(Reference("DEAL-2026-0003".into()))
        .get_one()
        .await?;
    assert_eq!(live.stage, Stage::Won);
    Ok(())
}

async fn asking_for_one_deal_where_several_match<U: Under>() -> Result<(), DocumentError> {
    let (conn, _) = seeded::<U>().await?;

    assert_eq!(
        Deal::query(&conn).by_owner(Rep::Ana).get_one().await,
        Err(DocumentError::NotUnique)
    );
    assert_eq!(
        Deal::query(&conn)
            .by_reference(Reference("DEAL-9999-9999".into()))
            .get_one()
            .await,
        Err(DocumentError::NotFound)
    );
    Ok(())
}

/// An enum column encodes its variant name, so it sorts alphabetically rather
/// than by pipeline position. Use one for equality and keep an integer if the
/// order has to mean something.
#[test]
fn an_enum_column_orders_by_name_not_by_position() {
    let mut stages = [Stage::Lead, Stage::Qualified, Stage::Proposal, Stage::Won];
    stages.sort_by_key(|stage| stage.to_ord_bytes());
    assert_eq!(
        stages,
        [Stage::Lead, Stage::Proposal, Stage::Qualified, Stage::Won]
    );
}

async fn the_indexes_the_attributes_asked_for<U: Under>() -> Result<(), DocumentError> {
    let names: Vec<&str> = Deal::schemas().iter().map(|schema| schema.id.0).collect();
    assert_eq!(
        names,
        [
            "deal_latest",
            "deal_by_reference",
            "deal_by_account",
            "deal_by_amount_cents",
            "deal_by_owner_stage"
        ],
        "every record is filed under its own identity first"
    );

    let arities: Vec<usize> = Deal::schemas()
        .iter()
        .map(|schema| schema.arity())
        .collect();
    assert_eq!(
        arities,
        [2, 1, 2, 2, 3],
        "the generated index is (document, revision); only a unique one skips the id"
    );

    assert_eq!(Deal::LATEST.id().0, "deal_latest");
    assert_eq!(Deal::BY_REFERENCE.id().0, "deal_by_reference");
    assert_eq!(Deal::BY_ACCOUNT.id().0, "deal_by_account");
    assert_eq!(Deal::BY_AMOUNT_CENTS.id().0, "deal_by_amount_cents");
    assert_eq!(Deal::BY_OWNER_STAGE.id().0, "deal_by_owner_stage");

    assert_eq!(Account::LATEST.arity(), 2);
    assert_eq!(Account::BY_TAX_ID.arity(), 1);
    assert_eq!(Account::BY_OWNER.arity(), 2);
    assert_eq!(Account::BY_COUNTRY_CITY.arity(), 3);

    Ok(())
}

/// Counts what reaches the engine, so a claim about round trips is checked
/// rather than described.
struct Counted<E> {
    inner: E,
    reads: AtomicUsize,
}

impl<E> Counted<E> {
    fn new(inner: E) -> Self {
        Self {
            inner,
            reads: AtomicUsize::new(0),
        }
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl<E: Engine> Engine for Counted<E> {
    async fn define(&self, indexes: &[IndexSchema]) -> Result<(), StorageError> {
        self.inner.define(indexes).await
    }

    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read(reads).await
    }

    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError> {
        self.inner.apply(batch).await
    }
}

/// The reason `load_many` exists: following a set costs what following one
/// costs, because they are all rows in one ordered index.
async fn following_a_set_of_references_costs_one_scan<U: Under>() -> Result<(), DocumentError> {
    let conn = Connection::with_clock(
        Counted::new(U::engine().await),
        &schemas(),
        FixedClock(Timestamp(EPOCH_MS + 1_000)),
        NodeId::new(1).expect("one fits in ten bits"),
    )
    .await?;

    let mut held = Vec::new();
    for tax in ["80000000-1", "80000000-2", "80000000-3"] {
        held.push(
            conn.insert(account(
                tax,
                "A Company",
                Country::Paraguay,
                "Asuncion",
                Rep::Ana,
            ))
            .await?
            .reference(),
        );
    }

    let before = conn.db().engine().reads();
    let one = conn.load(held[0]).await?;
    let for_one = conn.db().engine().reads() - before;

    let before = conn.db().engine().reads();
    let all = conn.load_many(held.clone()).await?;
    let for_all = conn.db().engine().reads() - before;

    assert_eq!(all.len(), 3, "every reference resolved");
    assert_eq!(one.id(), held[0].id());
    assert_eq!(
        for_all, for_one,
        "three references cost what one costs, not three times it"
    );
    Ok(())
}

on_every_engine! {
    #[tokio::test]
    the_deals_on_one_account,
    one_reps_pipeline_at_one_stage,
    a_foreign_key_survives_an_edit_of_what_it_points_at,
    deals_above_a_threshold,
    a_credit_note_sorts_below_zero,
    the_account_behind_a_deal,
    following_a_reference_answers_the_live_revision,
    following_every_deals_account_at_once,
    following_no_references_reads_nothing,
    a_reference_to_a_record_that_is_not_there_is_not_found,
    accounts_in_one_city,
    advancing_a_deal_files_a_new_revision,
    a_deals_history_is_every_stage_it_passed_through,
    a_reference_is_taken_once,
    two_reps_editing_one_deal,
    asking_for_one_deal_where_several_match,
    the_indexes_the_attributes_asked_for,
    following_a_set_of_references_costs_one_scan,
}
