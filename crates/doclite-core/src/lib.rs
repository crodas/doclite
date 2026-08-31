//! Objects under an id, indexes over byte tuples, and transactions built as
//! values.
//!
//! An [`Operation`] is a transaction's worth of work: preconditions and
//! mutations in one ordered list, built up front and handed to [`Db::run`]
//! whole. Because it is a value rather than a closure, the engine can look at
//! it before running it, and can run it again after losing to a concurrent
//! writer.
//!
//! There is deliberately no `db.transaction(|tx| ..)`. A closure body cannot be
//! inspected for writes, so the engine loses its lock-mode choice, and it
//! cannot be safely re-run, so automatic retry goes with it.
//!
//! ```
//! use doclite_core::{exact, key, AbortCode, ObjectId, Index, Operation};
//!
//! static USERS_BY_EMAIL: Index<1> = Index::new("users_by_email");
//! const DUPLICATE_EMAIL: AbortCode = AbortCode::new(100);
//!
//! let id = ObjectId(7);
//! let op = Operation::new()
//!     .require_absent(&USERS_BY_EMAIL, exact(key!["a@b.c"]), DUPLICATE_EMAIL)
//!     .put_object(id, b"a blob".to_vec())
//!     .put_index(&USERS_BY_EMAIL, key!["a@b.c"], id)
//!     .returning(id);
//!
//! assert!(op.mutates());
//! op.check(&[USERS_BY_EMAIL.schema()]).expect("the operation is well formed");
//! ```
//!
//! Running it needs an engine, and what comes back is an answer rather than a
//! result: `Outcome::Committed(id)` the first time and
//! `Outcome::Aborted(DUPLICATE_EMAIL)` the second. See `doclite-memory` for
//! the whole loop.
//!
//! # Two features that both mention serde
//!
//! They are about different halves of the crate and neither implies the other.
//! `serde` derives `Serialize` on the operation representation, so an operation
//! could be encoded and shipped. `cbor` and `json` are the two object codecs,
//! bringing `serde_storage!` and `json_storage!`, which implement [`ToStorage`]
//! and [`FromStorage`] for a type that already derives serde's two traits. Not
//! links, because a macro is only there when its feature is. The two are
//! additive: a record picks one, and two records need not agree.

pub mod engine;

#[cfg(any(feature = "cbor", feature = "json"))]
#[doc(hidden)]
pub mod reexport;

mod check;
mod db;
mod executor;
mod fake;
mod model;
mod operation;
mod outcome;
mod query;

pub use crate::db::{Db, Runnable};
pub use crate::engine::{
    Answers, Applied, Assertion, Batch, Engine, IndexEntry, IndexRead, KeyRange, Observed, Plan,
    RangeDelete, Reads, Sweep,
};
pub use crate::model::column::{Column, IntoColumn, IntoObject};
pub use crate::model::encoding::ToOrdBytes;
pub use crate::model::index::{ColumnType, Index, IndexId, IndexSchema};
pub use crate::model::key::Key;
pub use crate::model::object::{ObjectId, ObjectRef};
pub use crate::model::range::{all, between, bounded, exact, prefix, Bound, Range, RangeSpec};
pub use crate::model::row::{Row, RowKey};
pub use crate::model::storage::{FromStorage, ToStorage};
pub use crate::operation::step::{Expect, Step};
pub use crate::operation::template::{Cell, ObjectCell, RowRef};
pub use crate::operation::Operation;
pub use crate::outcome::abort::AbortCode;
pub use crate::outcome::error::{BuildError, CodecError, StorageError};
pub use crate::outcome::Outcome;
pub use crate::query::plan::{ObjectMode, ReadPlan};
pub use crate::query::value::{FromReadValue, ReadValue};
pub use crate::query::Query;
