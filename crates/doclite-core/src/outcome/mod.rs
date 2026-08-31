//! How work reports back: a value, a refusal, or a failure.
//!
//! The three are deliberately not one type. An [`Outcome`] is an answer, and a
//! refused precondition is one of its variants rather than an error;
//! [`error::StorageError`] is the storage layer failing to give an answer at
//! all.

pub(crate) mod abort;
pub(crate) mod error;

use crate::outcome::abort::AbortCode;

/// What running an operation or a query produced.
///
/// The split against [`error::StorageError`] is the one the design turns on: a
/// duplicate email is `Ok(Aborted(..))` because the operation did its job, and
/// a failed write is `Err` because it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    /// The work ran and its writes landed.
    Committed(T),
    /// A precondition did not hold, so nothing was written.
    Aborted(AbortCode),
}

impl<T> Outcome<T> {
    /// The value, dropping which code refused, for a caller that only cares
    /// whether it got one.
    pub fn committed(self) -> Option<T> {
        match self {
            Self::Committed(value) => Some(value),
            Self::Aborted(_) => None,
        }
    }

    /// Which code refused, for matching on it against the caller's own
    /// constants.
    pub fn aborted(&self) -> Option<AbortCode> {
        match self {
            Self::Committed(_) => None,
            Self::Aborted(code) => Some(*code),
        }
    }

    /// Whether the work landed, without taking the value out.
    pub fn is_committed(&self) -> bool {
        matches!(self, Self::Committed(_))
    }

    /// Changes the committed value and carries a refusal through untouched, so
    /// a layer can retype an answer without deciding what a refusal meant.
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Outcome<U> {
        match self {
            Self::Committed(value) => Outcome::Committed(f(value)),
            Self::Aborted(code) => Outcome::Aborted(code),
        }
    }
}
