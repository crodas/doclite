//! A typed document layer over `doclite-core`.
//!
//! `#[derive(Document)]` turns a struct into its own indexes, queries and
//! revision history. A document is a chain of revisions under one id that never
//! moves; `doclite-core` still holds each revision as bytes under an `ObjectId`
//! and knows nothing about any of it.
//!
//! ```
//! use doclite::{Column, Connection, Document, DocumentError, Record};
//! use doclite_memory::MemoryEngine;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
//! struct Email(String);
//!
//! #[derive(Debug, Clone, Serialize, Deserialize, Document)]
//! #[index(city, age)]
//! struct User {
//!     #[unique]
//!     email: Email,
//!     city: String,
//!     #[index]
//!     age: u8,
//! }
//!
//! # async fn example() -> Result<(), DocumentError> {
//! let conn = Connection::open(MemoryEngine::new(), &User::schemas()).await?;
//!
//! let first = conn
//!     .insert(User {
//!         email: Email("a@b.c".into()),
//!         city: "Asuncion".into(),
//!         age: 24,
//!     })
//!     .await?;
//!
//! let adults = User::query(&conn).by_age(18..).limit(10).get_many().await?;
//! assert_eq!(adults.len(), 1);
//!
//! let mut user = User::query(&conn)
//!     .by_email(Email("a@b.c".into()))
//!     .get_one()
//!     .await?;
//! user.age = 25;
//! let now = conn.update(user).await?;
//!
//! assert_eq!(now.age, 25);
//! assert!(now.prev().is_some(), "the revision it superseded");
//! assert_ne!(now.revision(), first.revision(), "a new revision");
//! assert_eq!(now.id(), first.id(), "the same user");
//! # Ok(())
//! # }
//! # tokio::runtime::Builder::new_current_thread()
//! #     .build()
//! #     .expect("a current-thread runtime")
//! #     .block_on(example())
//! #     .expect("the example runs");
//! ```
//!
//! # Choosing an encoding
//!
//! A record's bytes are CBOR unless it says otherwise. `#[storage(json)]` makes
//! them JSON, which costs size and buys a store that can be read without this
//! crate: under `doclite-sqlite` such a record lands in `doclite_json_document`
//! as a text value, so `sqlite3` prints the document and `json_extract` reads a
//! field out of it in place. The choice is per record, so two records in one
//! database may disagree and nothing below the document layer is told either
//! way.
//!
//! The whole object is one JSON value, revision envelope included, because a
//! binary header in front of a JSON body would leave it neither. The record
//! nests under `body`, so `json_extract(body, '$.body.email')` reads a field and
//! `'$.prev'` reads the revision this one superseded.
//!
//! ```
//! use doclite::{Connection, Document, DocumentError, Record};
//! use doclite_memory::MemoryEngine;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Clone, Serialize, Deserialize, Document)]
//! #[storage(json)]
//! struct Note {
//!     #[unique]
//!     slug: String,
//!     body: String,
//! }
//!
//! # async fn example() -> Result<(), DocumentError> {
//! let conn = Connection::open(MemoryEngine::new(), &Note::schemas()).await?;
//! let note = conn
//!     .insert(Note {
//!         slug: "n-1".into(),
//!         body: "the first one".into(),
//!     })
//!     .await?;
//! assert_eq!(note.slug, "n-1");
//! # Ok(())
//! # }
//! # tokio::runtime::Builder::new_current_thread()
//! #     .build()
//! #     .expect("a current-thread runtime")
//! #     .block_on(example())
//! #     .expect("the example runs");
//! ```
//!
//! A record carrying bytes cannot ask for JSON, because `serde_json` writes a
//! `Vec<u8>` as an array of numbers: it round-trips and is unreadable, which is
//! the one thing JSON was chosen for. Such a record is refused rather than
//! quietly stored that way, and dropping the attribute stores it as CBOR:
//!
//! ```compile_fail
//! use doclite::Document;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Clone, Serialize, Deserialize, Document)]
//! #[storage(json)]
//! struct Scan {
//!     #[unique]
//!     reference: String,
//!     image: Vec<u8>,
//! }
//! ```
//!
//! The check reads the field's written type, so a `type Blob = Vec<u8>` alias
//! or a newtype around one goes through. It catches what gets written rather
//! than everything that could be.

pub use doclite_core;
pub use doclite_core::{json_storage, serde_storage};
pub use doclite_derive::{Column, Document};

pub mod bounds;
pub mod clock;
pub mod connection;
pub mod error;
pub mod id;
pub mod identity;
pub mod query;
pub mod record;
pub mod revision;
pub mod transaction;
mod write;

pub use crate::bounds::{greater_than, Bounds, Comparison, IntoBounds};
pub use crate::clock::{Clock, FixedClock, SystemClock, Timestamp};
pub use crate::connection::Connection;
pub use crate::error::DocumentError;
pub use crate::id::{IdError, IdSource, NodeId, Snowflake};
pub use crate::identity::{DocumentId, Ref, RevisionId};
pub use crate::query::{scan_between, scan_prefix, scan_within, Selection};
pub use crate::record::{Amendable, Record, Rows};
pub use crate::revision::{Revision, Stored};
pub use crate::transaction::Transaction;
