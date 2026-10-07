//! The SQLite engine for DocLite.
//!
//! It implements [`Engine`] rather than a store, and not only because ADR 0011
//! routes a backend with its own query language here. A store's writes are
//! infallible and it has no commit point, so a `COMMIT` that can fail would
//! have to happen as the region drops, after success had already been returned.
//! `docs/sqlite.md` is the design and `docs/adr/0013-a-sqlite-engine.md` is why.
//!
//! [`Engine`]: doclite_core::Engine

mod apply;
mod catalog;
mod connection;
mod define;
mod engine;
mod held;
mod plan;
mod read;
mod statement;
mod storage;
mod sweep;
mod version;

pub use crate::engine::{Durability, SqliteEngine};
