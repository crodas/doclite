//! Landing a whole batch, or none of it.
//!
//! The order is the contract and it is not the order of `Batch::Writes`'s
//! fields: every assertion against pre-batch state, then the checks that refuse
//! the batch outright, then deletes, then objects, then index rows.
//!
//! A sweep is the other variant rather than a fifth step, so where it goes in
//! that order is not a question this file has to answer.
//!
//! A failed assertion is `Applied::Conflict` and not an error, because the
//! executor discards its delta and walks the operation again. A duplicate
//! object is an error, because walking again cannot fix it.
//!
//! `crates/doclite-store/src/admit.rs` is this protocol against a region. It is
//! private and takes a `Region`, so this reproduces its order rather than
//! calling it.

use doclite_core::{Applied, Assertion, Batch, StorageError, Sweep};

use crate::connection::Open;
use crate::held::{Judging, Verdict, Writing};
use crate::sweep::Compiled;

pub(crate) fn apply(open: &mut Open, batch: &Batch<'_>) -> Result<Applied, StorageError> {
    let compiled = open.taken();

    match Judging::of(open)?.admits(batch)? {
        Verdict::Conflict => Ok(Applied::Conflict),
        Verdict::Admitted(writing) => {
            writing.write(batch, compiled.as_ref())?;
            writing.commit()?;
            Ok(Applied::Committed)
        }
    }
}

impl<'a> Judging<'a> {
    /// Whether this batch may land, asking everything that could refuse it
    /// before anything moves.
    ///
    /// A sweep has nothing to ask. It is the whole operation, so no assertion
    /// was recorded before it and no row is named until the statement runs.
    fn admits(self, batch: &Batch<'_>) -> Result<Verdict<'a>, StorageError> {
        let Batch::Writes {
            asserts,
            objects,
            delete,
            insert,
        } = batch
        else {
            return Ok(self.admitted());
        };

        for assertion in *asserts {
            if !self.holds(assertion)? {
                return Ok(Verdict::Conflict);
            }
        }

        for (id, _) in *objects {
            if self.has_object(*id)? {
                return Err(StorageError::DuplicateObject(*id));
            }
        }

        for delete in *delete {
            self.layout(delete.index)?;
        }

        for entry in *insert {
            let arity = self.layout(entry.index)?.arity();
            if entry.columns.len() != arity {
                return Err(StorageError::Arity {
                    index: entry.index,
                    expected: arity,
                    found: entry.columns.len(),
                });
            }
        }

        Ok(self.admitted())
    }

    /// Whether one assertion still describes the store.
    ///
    /// The comparison is `Observed::holds` rather than SQL. It applies the
    /// limit inside itself, taking the first rows of what it is given, so it is
    /// handed the whole range rather than a set already cut down. An engine
    /// that instead re-ran the range under its own limit would refuse batches
    /// that should land.
    fn holds(&self, assertion: &Assertion) -> Result<bool, StorageError> {
        match assertion {
            Assertion::Object { id, present } => Ok(self.has_object(*id)? == *present),
            Assertion::Index {
                index,
                range,
                limit,
                observed,
            } => {
                self.layout(*index)?;
                Ok(observed.holds(*limit, &self.every_row(*index, range)?))
            }
        }
    }
}

impl Writing<'_> {
    /// Everything the batch stages, in the order the contract puts it.
    fn write(
        &self,
        batch: &Batch<'_>,
        compiled: Option<&(Sweep, Compiled)>,
    ) -> Result<(), StorageError> {
        let (objects, delete, insert) = match batch {
            Batch::Sweep(sweep) => return self.sweep(sweep, compiled),
            Batch::Writes {
                objects,
                delete,
                insert,
                ..
            } => (objects, delete, insert),
        };

        for delete in *delete {
            self.retire(delete)?;
        }
        for (id, body) in *objects {
            self.file_object(*id, body)?;
        }
        for entry in *insert {
            self.file_row(entry)?;
        }
        Ok(())
    }
}
