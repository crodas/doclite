//! What a case reports when an engine does not hold up.
//!
//! A failure carries only what went wrong. Which case it was is the name of the
//! test `conformance!` generated, so nothing here repeats it.

use std::error::Error;
use std::fmt;

use doclite_core::StorageError;

/// How an engine failed the case that was running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// What went wrong, in full. Which case it was is the generated test's
    /// name, so this never repeats it.
    pub detail: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl Error for Failure {}

/// An engine that errors where the suite asked for an answer has failed the
/// case, whichever case was running.
impl From<StorageError> for Failure {
    fn from(error: StorageError) -> Self {
        fails(format!("answered with an error: {error}"))
    }
}

pub(crate) fn fails(detail: impl Into<String>) -> Failure {
    Failure {
        detail: detail.into(),
    }
}

pub(crate) fn expect(holds: bool, detail: impl Into<String>) -> Result<(), Failure> {
    if holds {
        Ok(())
    } else {
        Err(fails(detail))
    }
}
