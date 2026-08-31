//! The engine seam, worn over the connection thread.

use async_trait::async_trait;
use doclite_core::{
    Answers, Applied, Batch, Engine, IndexSchema, Plan, ReadValue, Reads, StorageError, Sweep,
};

use crate::connection::Connection;
use crate::{apply, define, plan, read, sweep};

/// Objects and indexes in SQLite, for a process that wants them to survive it.
///
/// Every method hands its work to one thread and waits, so a caller's runtime
/// is never blocked and never chosen for it.
/// How much a committed write is promised to survive.
///
/// Named for what it means rather than for the pragma underneath, because the
/// question a caller is answering is what they are willing to lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// It survives losing power, because the engine waits for the disk before
    /// calling a commit done.
    PowerLoss,
    /// It survives the process dying. The most recent commits may not survive
    /// the machine dying, which is what the waiting bought.
    ProcessCrash,
}

impl Durability {
    fn pragma(self) -> &'static str {
        match self {
            Self::PowerLoss => "FULL",
            Self::ProcessCrash => "NORMAL",
        }
    }
}

/// A durable engine over one SQLite database.
///
/// It owns a connection thread rather than choosing a runtime, and speaks SQL
/// for the reads it can push down. `docs/sqlite.md` is the design.
pub struct SqliteEngine {
    connection: Connection,
}

impl SqliteEngine {
    /// A database that lives as long as the engine does.
    ///
    /// This is what a test wants: it needs no path, no cleanup, and two of them
    /// are independent, which the conformance suite relies on.
    /// Nothing here reaches a disk, so the durability question does not arise
    /// and the pragma that answers it does nothing.
    pub fn in_memory() -> Result<Self, StorageError> {
        Self::over(
            rusqlite::Connection::open_in_memory(),
            Durability::PowerLoss,
        )
    }

    /// A database in a file, created if it is not there.
    ///
    /// Durable against losing power. That is the answer you get by not choosing
    /// one, because an engine whose purpose is durability should not quietly
    /// trade it away.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StorageError> {
        Self::open_with(path, Durability::PowerLoss)
    }

    /// A database in a file, promising only what was asked for.
    pub fn open_with(
        path: impl AsRef<std::path::Path>,
        durability: Durability,
    ) -> Result<Self, StorageError> {
        Self::over(rusqlite::Connection::open(path), durability)
    }

    /// Write-ahead logging so a reader and the writer stop blocking each other,
    /// and a busy timeout so a contended write waits rather than failing.
    ///
    /// The timeout is not a promise about what survives, only about how long a
    /// writer is willing to queue, so it is not the caller's to choose.
    fn over(
        opened: rusqlite::Result<rusqlite::Connection>,
        durability: Durability,
    ) -> Result<Self, StorageError> {
        let db = opened.map_err(crate::connection::failed)?;
        db.execute_batch(&format!(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = {}; PRAGMA busy_timeout = 5000;",
            durability.pragma()
        ))
        .map_err(crate::connection::failed)?;
        Ok(Self {
            connection: Connection::over(db),
        })
    }
}

#[async_trait]
impl Engine for SqliteEngine {
    async fn define(&self, indexes: &[IndexSchema]) -> Result<(), StorageError> {
        let indexes = indexes.to_vec();
        self.connection
            .run(move |open| define::define(open, &indexes))
            .await
    }

    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        let scans: Vec<_> = reads
            .scans
            .iter()
            .map(|scan| (scan.index, scan.range.clone(), scan.limit))
            .collect();
        let objects = reads.objects.to_vec();
        self.connection
            .run(move |open| read::read(open, &scans, &objects))
            .await
    }

    /// The batch is copied because it crosses to the connection's thread, and
    /// it is copied as the shape it arrived in.
    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError> {
        match batch {
            Batch::Writes {
                asserts,
                objects,
                delete,
                insert,
            } => {
                let asserts = asserts.to_vec();
                let objects = objects.to_vec();
                let delete = delete.to_vec();
                let insert = insert.to_vec();
                self.connection
                    .run(move |open| {
                        apply::apply(
                            open,
                            &Batch::Writes {
                                asserts: &asserts,
                                objects: &objects,
                                delete: &delete,
                                insert: &insert,
                            },
                        )
                    })
                    .await
            }
            Batch::Sweep(sweep) => {
                let sweep = (*sweep).clone();
                self.connection
                    .run(move |open| apply::apply(open, &Batch::Sweep(&sweep)))
                    .await
            }
        }
    }

    /// Every plan, because SQLite can express all four shapes and answering
    /// them here is the reason to use a backend with a planner at all.
    ///
    /// One transaction spans the whole plan, so a zip agrees with itself and
    /// the objects a scan asked for come from the same view as its rows.
    async fn plan(&self, wanted: &Plan) -> Result<Option<ReadValue>, StorageError> {
        let wanted = wanted.clone();
        self.connection
            .run(move |open| plan::plan(open, &wanted).map(Some))
            .await
    }

    /// Whether the whole `for_each` compiles to statements.
    ///
    /// Compiling it is the answer. A template this cannot write is expanded by
    /// the executor instead, which is the same work done a slower way rather
    /// than a refusal the caller ever sees.
    ///
    /// A connection that cannot answer declines as well. The error is not lost:
    /// the expansion that follows reads the same database and reports it.
    async fn takes_sweep(&self, sweep: &Sweep) -> bool {
        let sweep = sweep.clone();
        self.connection
            .run(move |open| Ok(sweep::takes(open, &sweep)))
            .await
            .unwrap_or(false)
    }
}
