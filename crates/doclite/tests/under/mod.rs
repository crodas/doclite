//! Which engine a connection is under, so a suite can be run on every one.
//!
//! The document layer's answer to `doclite_testkit::Fresh`, and the same
//! argument: the engine seam is proved on both engines by `conformance!`, and
//! nothing above it was. `<record>_latest`, a refused unique key, a `Ref`
//! followed through `load_many` and the abort codes a transaction numbers are
//! all this layer's, and a scenario that only ever ran in memory says nothing
//! about the engine that outlives the process.
//!
//! Async to match `Fresh`, though neither constructor here needs it. An engine
//! that opens a connection is the reason that trait is async, and this one will
//! meet the same engine eventually.

use async_trait::async_trait;
use doclite::doclite_core::Engine;
use doclite_memory::MemoryEngine;
use doclite_sqlite::SqliteEngine;

/// An engine a scenario can be run against.
#[async_trait]
pub trait Under {
    /// The engine this builds.
    ///
    /// `'static` because the contention scenarios share a connection across
    /// spawned tasks, which nothing else here needs.
    type Engine: Engine + 'static;

    /// A new engine holding nothing, one per scenario.
    async fn engine() -> Self::Engine;
}

pub struct Memory;

#[async_trait]
impl Under for Memory {
    type Engine = MemoryEngine;

    async fn engine() -> MemoryEngine {
        MemoryEngine::new()
    }
}

pub struct Sqlite;

/// In memory rather than over a file, because what changes between the two is
/// the write-ahead log and fsync, and `doclite-sqlite/tests/durable.rs` already
/// covers those at the seam where they live. The SQL this layer drives is the
/// same either way.
#[async_trait]
impl Under for Sqlite {
    type Engine = SqliteEngine;

    async fn engine() -> SqliteEngine {
        SqliteEngine::in_memory().expect("an in-memory database opens")
    }
}

/// Expands each scenario into one test per engine.
///
/// Modelled on `doclite_testkit::conformance!`, and the same reason: the
/// harness is the walk, so an engine that breaks four scenarios reports four
/// failures rather than the first. The two modules are what let
/// `cargo test on_sqlite` run one engine's half.
///
/// Every scenario is generic over [`Under`] and answers
/// `Result<(), DocumentError>`, which the invoking file has in scope.
///
/// The test attribute is the caller's to pass, as `conformance!` does it,
/// because a suite that has to race needs a runtime with more than one thread
/// and the rest of them do not.
macro_rules! on_every_engine {
    (#[$attr:meta] $($scenario:ident),* $(,)?) => {
        mod on_memory {
            $(
                #[$attr]
                async fn $scenario() -> Result<(), super::DocumentError> {
                    super::$scenario::<crate::under::Memory>().await
                }
            )*
        }

        mod on_sqlite {
            $(
                #[$attr]
                async fn $scenario() -> Result<(), super::DocumentError> {
                    super::$scenario::<crate::under::Sqlite>().await
                }
            )*
        }
    };
}

pub(crate) use on_every_engine;
