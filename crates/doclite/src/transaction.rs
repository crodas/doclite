//! Several records filed as one operation.
//!
//! [`Connection::insert`] and [`Connection::update`] are each one operation,
//! which is the right unit for one record and the wrong one for a change that
//! is only correct as a whole: a task written without the state transition that
//! justifies it is worse than neither.
//!
//! Nothing here is a new kind of write. A transaction builds exactly the
//! operations those two methods build and joins them with
//! [`Operation::then`](doclite_core::Operation::then), which is why there is no
//! `begin`, no `rollback`, and no way to hold one open across an await: an
//! operation is a value (ADR 0003), and the value is complete or it is not
//! handed over.
//!
//! What the joining costs is the numbering. A write's abort codes say why it
//! refused, and two writes numbered from the same place would both answer to
//! the same code, so an abort could not name which record refused. Each staged
//! write is therefore given the codes after the last one's, and the run it owns
//! is what resolves the answer.

use doclite_core::{Engine, Operation, Outcome};

use crate::clock::Clock;
use crate::connection::Connection;
use crate::error::DocumentError;
use crate::id::minted_at;
use crate::record::{Amendable, Codes, Record, CODE_BASE};
use crate::revision::{Revision, Stored};
use crate::write::{self, Refusals};

/// Records staged for one operation.
///
/// Staging mints the ids and builds the steps; nothing reaches the store until
/// [`Transaction::commit`]. A transaction that is dropped instead writes
/// nothing, because there was never anything but a value.
pub struct Transaction<'a, E, C> {
    connection: &'a Connection<E, C>,
    operation: Operation,
    staged: Vec<Refusals>,
    next: u32,
}

impl<'a, E: Engine, C: Clock> Transaction<'a, E, C> {
    pub(crate) fn opening(connection: &'a Connection<E, C>) -> Self {
        Self {
            connection,
            operation: Operation::new(),
            staged: Vec::new(),
            next: CODE_BASE,
        }
    }

    /// Stages a new record, exactly as [`Connection::insert`] would file it.
    ///
    /// The [`Stored`] comes back before the commit, because the id is minted
    /// here rather than by the store. A refused commit means the caller
    /// discards it: nothing was written under it.
    pub fn insert<T: Record>(&mut self, value: T) -> Result<Stored<T>, DocumentError> {
        let revision = self.connection.mint()?;
        let document = revision.opens();
        let envelope = Revision::new(value, document, minted_at(revision), None);

        let write = write::insert(&envelope, document, revision, Codes::at(self.next));
        self.stage(write);

        Ok(Stored::new(revision, envelope))
    }

    /// Stages an edit, exactly as [`Connection::update`] would supersede it.
    ///
    /// A record the schema marked `#[immutable]` does not implement
    /// [`Amendable`] and so cannot be passed here, the same compile error it is
    /// on [`Connection::update`].
    pub fn update<T: Record + Amendable>(
        &mut self,
        stored: Stored<T>,
    ) -> Result<Stored<T>, DocumentError> {
        let (document, old, edited, now) = stored.into_parts();
        let new = self.connection.mint()?;
        let envelope = Revision::new(now, document, minted_at(new), Some(old));
        let was: &T = edited.as_ref().unwrap_or(&envelope);

        let write = write::supersede(&envelope, was, document, old, new, Codes::at(self.next));
        self.stage(write);

        Ok(Stored::new(new, envelope))
    }

    /// How many records are staged, which is what makes an empty commit
    /// answerable without running one.
    pub fn len(&self) -> usize {
        self.staged.len()
    }

    /// Whether committing would write anything.
    pub fn is_empty(&self) -> bool {
        self.staged.is_empty()
    }

    /// Files everything staged, or nothing.
    ///
    /// A refusal is reported as the record that caused it: the code an abort
    /// comes back with falls inside exactly one staged write's run, and that
    /// write is the one asked to name it.
    pub async fn commit(self) -> Result<(), DocumentError> {
        if self.staged.is_empty() {
            return Ok(());
        }

        match self.connection.gated(&self.operation).await? {
            Outcome::Committed(()) => Ok(()),
            Outcome::Aborted(code) => Err(self
                .staged
                .iter()
                .find(|staged| staged.owns(code))
                .map_or(DocumentError::Aborted(code), |staged| staged.refusal(code))),
        }
    }

    fn stage(&mut self, write: write::Write) {
        let (operation, refusals) = write.split();
        self.next += refusals.span();
        self.staged.push(refusals);

        let built = std::mem::replace(&mut self.operation, Operation::new());
        self.operation = built.then(operation);
    }
}
