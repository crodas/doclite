//! The smallest complete program: define a record, file it, find it, change it.
//!
//! ```sh
//! cargo run -p doclite --example quickstart
//! ```

use doclite::{records, Column, Connection, Document, DocumentError};
use doclite_memory::MemoryEngine;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Title(String);

#[derive(Debug, Clone, Serialize, Deserialize, Document)]
struct Task {
    /// No two tasks may share a title. Nothing declares that on the index; the
    /// operation asserts it, and a second insert is refused.
    #[unique]
    title: Title,

    #[index]
    priority: u8,

    done: bool,
}

fn task(title: &str, priority: u8) -> Task {
    Task {
        title: Title(title.into()),
        priority,
        done: false,
    }
}

#[tokio::main]
async fn main() -> Result<(), DocumentError> {
    let conn = Connection::open(MemoryEngine::new(), records![Task]).await?;

    conn.insert(task("Write the release notes", 2)).await?;
    conn.insert(task("Fix the flaky test", 1)).await?;
    conn.insert(task("Answer the mailing list", 5)).await?;

    println!("everything, urgent first:");
    for found in Task::query(&conn).by_priority(0..).get_many().await? {
        println!("  {}  {}", found.priority, found.title.0);
    }

    println!("\nonly the urgent ones (priority < 3):");
    for found in Task::query(&conn).by_priority(..3).get_many().await? {
        println!("  {}  {}", found.priority, found.title.0);
    }

    println!("\na title is taken once:");
    match conn.insert(task("Fix the flaky test", 9)).await {
        Err(DocumentError::Duplicate { index }) => println!("  refused by {index}"),
        other => println!("  unexpected: {other:?}"),
    }

    let mut editing = Task::query(&conn)
        .by_title(Title("Fix the flaky test".into()))
        .get_one()
        .await?;
    editing.done = true;
    editing.priority = 0;
    let now = conn.update(editing).await?;

    println!("\nafter marking it done:");
    println!("  priority {}, done {}", now.priority, now.done);
    println!("  the task is  {}", now.id());
    println!("  this revision is {}", now.revision());
    if let Some(prev) = now.prev() {
        println!("  it superseded  {prev}");
    }

    println!("\nwhat it looked like before:");
    for older in conn.history(&now).await? {
        println!("  priority {}, done {}", older.priority, older.done);
    }

    Ok(())
}
