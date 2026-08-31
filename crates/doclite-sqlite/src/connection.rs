//! The one thread that touches the database.
//!
//! `rusqlite::Connection` is `Send` and not `Sync`, and `Engine` takes `&self`
//! and returns a `Send` future, so a connection cannot simply sit behind a
//! shared reference. ADR 0006 keeps the choice of runtime with the caller,
//! which rules out a blocking pool belonging to one runtime and an async driver
//! that picks one through a feature.
//!
//! So the connection lives on a thread this owns, and every method sends work
//! down a channel and waits on a reply that belongs to no runtime. Nothing else
//! ever holds the connection, which is what satisfies `Send + Sync` without a
//! lock and without a guard held across an await.
//!
//! Reads serialize behind writes while there is one connection. WAL allows
//! concurrent readers on separate connections, so a reader pool is the obvious
//! later change and deliberately not the first one.

use std::sync::mpsc::{self, Sender};
use std::thread;

use doclite_core::{StorageError, Sweep};
use futures_channel::oneshot;

use crate::catalog::Catalog;
use crate::sweep::Compiled;

/// What the connection thread owns.
///
/// The catalog lives here rather than beside the engine so that neither needs a
/// lock: the only code that can see it is already the only code that can see
/// the connection.
pub(crate) struct Open {
    db: rusqlite::Connection,
    catalog: Catalog,
    /// What the last `takes_sweep` said yes to.
    ///
    /// The executor asks whether a sweep can be written and then immediately
    /// applies it, so remembering the one answer means it is compiled once
    /// rather than once per question.
    compiled: Option<(Sweep, Compiled)>,
}

impl Open {
    /// The connection and the catalog at once.
    ///
    /// Handed out together because a transaction borrows one and reads the
    /// other for as long as it lives, which one accessor at a time cannot
    /// express.
    pub(crate) fn parts(&mut self) -> (&mut rusqlite::Connection, &mut Catalog) {
        (&mut self.db, &mut self.catalog)
    }

    pub(crate) fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Keeps what a sweep compiled to, or forgets the last one when it did not
    /// compile at all.
    pub(crate) fn remember(&mut self, compiled: Option<(Sweep, Compiled)>) {
        self.compiled = compiled;
    }

    /// The memo, and clearing it, because a compiled sweep answers once.
    pub(crate) fn taken(&mut self) -> Option<(Sweep, Compiled)> {
        self.compiled.take()
    }
}

type Job = Box<dyn FnOnce(&mut Open) + Send>;

/// A handle on the thread, and the only way to reach the database.
pub(crate) struct Connection {
    jobs: Sender<Job>,
}

impl Connection {
    /// Moves a connection onto its own thread.
    ///
    /// The thread ends when the last handle is dropped and the channel closes,
    /// so nothing has to be shut down by hand.
    pub(crate) fn over(db: rusqlite::Connection) -> Self {
        let (jobs, queue) = mpsc::channel::<Job>();
        thread::spawn(move || {
            let mut open = Open {
                db,
                catalog: Catalog::default(),
                compiled: None,
            };
            while let Ok(job) = queue.recv() {
                job(&mut open);
            }
        });
        Self { jobs }
    }

    /// Hands work to the thread and waits for what it answered.
    ///
    /// A reply that cannot be delivered means the caller stopped waiting. That
    /// is the only way the send fails, and it is not a failure of the work, so
    /// the answer is dropped rather than reported.
    pub(crate) async fn run<T, F>(&self, work: F) -> Result<T, StorageError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Open) -> Result<T, StorageError> + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |open| match reply.send(work(open)) {
            Ok(()) => {}
            Err(_undelivered) => {}
        });

        self.jobs
            .send(job)
            .map_err(|_| StorageError::Engine("the connection thread has stopped".into()))?;

        answer
            .await
            .map_err(|_| StorageError::Engine("the connection thread dropped the answer".into()))?
    }
}

/// What a `rusqlite` failure looks like once it leaves this crate.
///
/// `StorageError::Engine` is the one place a string is carried, and a backend
/// condition is exactly what it is for.
pub(crate) fn failed(error: rusqlite::Error) -> StorageError {
    StorageError::Engine(error.to_string())
}
