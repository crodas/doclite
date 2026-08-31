//! The batch protocol, written once, for engines that keep ordered rows.
//!
//! There are two ways to be an engine, and ADR 0011 is why. A store with a
//! query language of its own implements [`Engine`](doclite_core::Engine)
//! directly and admits a batch however it likes, which is the freedom ADR 0004
//! granted. A store that keeps ordered rows and nothing more implements
//! [`Store`], and [`StoreEngine`] supplies the rest: what an assertion means,
//! when a batch is refused, and the order a batch lands in.
//!
//! ```text
//! Engine ─┬─ SQLite, Postgres      implement Engine, use their own SQL
//!         └─ StoreEngine<S> ─── S  implement Store, keep ordered rows
//! ```
//!
//! Before this, every engine wrote the protocol out again, and the copies could
//! not see each other. Assertions are recorded by the executor and re-evaluated
//! here, and those two halves now share one definition in
//! `doclite_core::Observed`.

mod admit;
mod engine;
mod store;

pub use engine::StoreEngine;
pub use store::{Region, Store};
