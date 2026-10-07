//! Two records that reference each other, a pipeline to report on, and a deal
//! that moves through stages.
//!
//! ```sh
//! cargo run -p doclite --example crm
//! ```

use doclite::{
    greater_than, records, Column, Connection, Document, DocumentError, Records, Ref, Stored,
};
use doclite_memory::MemoryEngine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct TaxId(String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Reference(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Column)]
enum Country {
    Argentina,
    Paraguay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Column)]
enum Rep {
    Ana,
    Bruno,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Column)]
enum Stage {
    Lead,
    Qualified,
    Proposal,
    Won,
    Lost,
}

#[derive(Debug, Clone, Serialize, Deserialize, Document)]
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

#[derive(Debug, Clone, Serialize, Deserialize, Document)]
#[index(owner, stage)]
struct Deal {
    #[unique]
    reference: Reference,

    /// A `Ref` is an index column like any other, so the join back to an
    /// account is a scan of one index.
    #[index]
    account: Ref<Account>,

    owner: Rep,

    stage: Stage,

    /// Money as an integer of the smallest unit, signed so a credit note sorts
    /// below every sale rather than above them.
    #[index]
    amount_cents: i64,
}

fn catalog() -> Records {
    records![Account, Deal]
}

/// Derived `Debug` ignores a width, so a column of stages has to be a string
/// before it can be padded.
fn stage(stage: Stage) -> String {
    format!("{stage:?}")
}

/// Guaranies, which have no minor unit in practice, printed with separators.
fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let digits = cents.abs().to_string();
    let mut grouped = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(digit);
    }
    format!("{sign}{grouped}")
}

async fn seed(
    conn: &Connection<MemoryEngine>,
) -> Result<(Stored<Account>, Stored<Account>), DocumentError> {
    let roma = conn
        .insert(Account {
            tax_id: TaxId("80012345-6".into()),
            name: "Clinica Roma".into(),
            country: Country::Paraguay,
            city: "Asuncion".into(),
            owner: Rep::Ana,
        })
        .await?;

    let sur = conn
        .insert(Account {
            tax_id: TaxId("80098765-4".into()),
            name: "Distribuidora Sur".into(),
            country: Country::Paraguay,
            city: "Encarnacion".into(),
            owner: Rep::Bruno,
        })
        .await?;

    let deals = [
        (
            "DEAL-2026-0001",
            roma.reference(),
            Rep::Ana,
            Stage::Proposal,
            4_500_000,
        ),
        (
            "DEAL-2026-0002",
            roma.reference(),
            Rep::Ana,
            Stage::Qualified,
            900_000,
        ),
        (
            "DEAL-2026-0003",
            sur.reference(),
            Rep::Bruno,
            Stage::Proposal,
            12_000_000,
        ),
        (
            "DEAL-2026-0004",
            sur.reference(),
            Rep::Bruno,
            Stage::Won,
            2_000_000,
        ),
        (
            "DEAL-2026-0005",
            roma.reference(),
            Rep::Ana,
            Stage::Won,
            -750_000,
        ),
    ];

    for (reference, account, owner, stage, amount_cents) in deals {
        conn.insert(Deal {
            reference: Reference(reference.into()),
            account,
            owner,
            stage,
            amount_cents,
        })
        .await?;
    }

    Ok((roma, sur))
}

#[tokio::main]
async fn main() -> Result<(), DocumentError> {
    let conn = Connection::open(MemoryEngine::new(), catalog()).await?;
    let (roma, _sur) = seed(&conn).await?;

    println!("Ana's book");
    for deal in Deal::query(&conn).by_owner(Rep::Ana).get_many().await? {
        println!(
            "  {}  {:<10} {:>12}",
            deal.reference.0,
            stage(deal.stage),
            money(deal.amount_cents)
        );
    }

    println!("\nAna's open proposals");
    for deal in Deal::query(&conn)
        .by_owner_stage(Rep::Ana, Stage::Proposal)
        .get_many()
        .await?
    {
        println!("  {}  {:>12}", deal.reference.0, money(deal.amount_cents));
    }

    println!("\ndeals over 2.000.000");
    for deal in Deal::query(&conn)
        .by_amount_cents(greater_than(2_000_000i64))
        .get_many()
        .await?
    {
        println!("  {}  {:>12}", deal.reference.0, money(deal.amount_cents));
    }

    println!("\ncredit notes, which sort below zero");
    for deal in Deal::query(&conn).by_amount_cents(..0).get_many().await? {
        println!("  {}  {:>12}", deal.reference.0, money(deal.amount_cents));
    }

    println!("\neverything on {}", roma.name);
    for deal in Deal::query(&conn)
        .by_account(roma.reference())
        .get_many()
        .await?
    {
        println!("  {}  {}", deal.reference.0, stage(deal.stage));
    }

    println!("\nfollowing each deal back to the account it names");
    let open = Deal::query(&conn).by_owner(Rep::Bruno).get_many().await?;
    let held = conn.load_many(open.iter().map(|deal| deal.account)).await?;
    println!(
        "  {} deals, {} account read in one scan",
        open.len(),
        held.len()
    );
    for account in &held {
        println!("  {}  ({})", account.name, account.city);
    }

    println!("\naccounts in Asuncion");
    for account in Account::query(&conn)
        .by_country_city(Country::Paraguay, "Asuncion".to_string())
        .get_many()
        .await?
    {
        println!("  {}  ({})", account.name, account.tax_id.0);
    }

    let find = || {
        Deal::query(&conn)
            .by_reference(Reference("DEAL-2026-0001".into()))
            .get_one()
    };

    let mut advancing = find().await?;
    advancing.amount_cents = 5_100_000;
    conn.update(advancing).await?;

    let mut closing = find().await?;
    closing.stage = Stage::Won;
    let latest = conn.update(closing).await?;

    println!(
        "\nDEAL-2026-0001 is now {} at {}",
        stage(latest.stage),
        money(latest.amount_cents)
    );
    println!("its history, newest first:");
    for older in conn.history(&latest).await? {
        println!(
            "  {:<10} {:>12}   revision {}",
            stage(older.stage),
            money(older.amount_cents),
            older.revision()
        );
    }

    println!("\ntwo reps editing the same deal:");
    let mut mine = find().await?;
    let mut theirs = find().await?;

    theirs.stage = Stage::Lost;
    conn.update(theirs).await?;

    mine.stage = Stage::Qualified;
    match conn.update(mine).await {
        Err(DocumentError::Stale) => {
            println!("  the second edit was refused, not silently applied")
        }
        other => println!("  unexpected: {other:?}"),
    }

    Ok(())
}
