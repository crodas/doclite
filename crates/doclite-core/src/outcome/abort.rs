//! Why an operation refused to commit.

use std::fmt;

/// The code a failing precondition reports.
///
/// Not an error. It is the operation reporting that a condition it was told to
/// check did not hold, which is an ordinary answer the caller matches on.
///
/// Callers define their own codes as associated constants on a type of their
/// own. The codes this crate reserves are the ones it raises itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AbortCode(pub u32);

impl AbortCode {
    /// Codes at or below this are this crate's to raise.
    pub const RESERVED: u32 = 15;

    /// A scan asked for objects and found an index row naming one that is
    /// not there.
    pub const DANGLING_INDEX_ENTRY: Self = Self(1);

    /// Only codes above [`AbortCode::RESERVED`] are the caller's to use.
    /// A lower one collides with a code this crate raises itself.
    pub const fn new(code: u32) -> Self {
        Self(code)
    }

    /// Whether this code is one this crate raises, and so not the caller's to
    /// give a meaning of its own.
    pub const fn is_reserved(&self) -> bool {
        self.0 <= Self::RESERVED
    }
}

impl fmt::Display for AbortCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "abort {}", self.0)
    }
}
