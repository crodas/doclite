//! Being told what changed, as a worked example.
//!
//! One account's deals, read from a checkpoint and then watched. The reader
//! holds nothing but the checkpoint between runs, which is the point: what it
//! missed is in the store rather than in a queue somewhere.
//!
//! ```text
//! cargo run --example feed
//! ```

use doclite::{records, Change, Checkpoint, Connection, Document, DocumentError, Ref, Stored};
use doclite_memory::MemoryEngine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Account {
    #[unique]
    name: String,
}

/// Owned by the account it belongs to and by whoever is watching it, which is
/// the one field `#[owner]` can go on. The account is the owner here; the
/// watchers are a second feed's worth of the same idea, so the record holds a
/// list of them instead.
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Deal {
    #[unique]
    reference: String,

    #[owner]
    #[index]
    account: Ref<Account>,

    amount_cents: i64,
}

/// A record several people own at once: one revision files a row in each of
/// their feeds.
#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Task {
    #[unique]
    title: String,

    #[owner]
    watchers: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), DocumentError> {
    let conn = Connection::open(MemoryEngine::new(), records![Account, Deal, Task]).await?;

    let roma = conn
        .insert(Account {
            name: "Clinica Roma".into(),
        })
        .await?;

    let mut deal = conn
        .insert(Deal {
            reference: "d-1".into(),
            account: roma.reference(),
            amount_cents: 100_000,
        })
        .await?;
    deal.amount_cents = 120_000;
    let deal = conn.update(deal).await?;

    println!("what happened while nobody was reading:");
    let missed = Deal::feed(&conn, roma.reference()).get_many().await?;
    for change in &missed {
        report(change);
    }

    let resume = missed.last().map_or_else(Checkpoint::origin, |last| {
        Checkpoint::after(last.revision())
    });

    let mut changes = Deal::feed(&conn, roma.reference())
        .since(resume)
        .watch()
        .await?;

    let mut deal = deal;
    deal.amount_cents = 90_000;
    let last = conn.update(deal).await?;

    println!("and what happened once it was:");
    while let Some(change) = changes.next().await {
        match change? {
            Change::Revised(deal) => {
                report(&deal);
                if deal.revision() == last.revision() {
                    break;
                }
            }
            Change::Lagged { from } => println!("  behind, reading again from {from:?}"),
        }
    }

    println!("come back with {:?}", changes.checkpoint());

    println!("and what several owners each hear:");
    let mut ana = Task::feed(&conn, "ana".into()).watch().await?;
    let mut bruno = Task::feed(&conn, "bruno".into()).watch().await?;

    conn.insert(Task {
        title: "call the lab".into(),
        watchers: vec!["ana".into(), "bruno".into()],
    })
    .await?;

    for (who, changes) in [("ana", &mut ana), ("bruno", &mut bruno)] {
        match changes.next().await {
            Some(Ok(Change::Revised(task))) => println!("  {who} heard about {}", task.title),
            Some(Ok(Change::Lagged { from })) => println!("  {who} is behind, from {from:?}"),
            Some(Err(why)) => return Err(why),
            None => println!("  {who} is not listening"),
        }
    }

    Ok(())
}

fn report(deal: &Stored<Deal>) {
    let kind = match deal.id() == deal.revision().opens() {
        true => "filed",
        false => "edited",
    };
    println!("  {kind} {} at {} cents", deal.reference, deal.amount_cents);
}
