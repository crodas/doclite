//! What names an object, and nothing about where a name comes from.

use std::fmt;

/// An object's identity, minted by whoever inserts it.
///
/// The storage layer decides nothing about where one comes from: monotonicity,
/// node identity and collision avoidance belong to the caller, who is also the
/// only party that can know what an object is.
///
/// Unlike [`IndexId`](crate::IndexId) this deserializes as well as serializes,
/// because it is a number rather than a borrowed name. That is what lets a
/// object hold another object's id as an ordinary field.
///
/// # The top bit is reserved
///
/// An id is sixty-three bits. [`ObjectId::LIMIT`] is the first value that is
/// not one, and an operation naming anything at or above it is refused by the
/// structural check before it reaches an engine.
///
/// The bit is not spare capacity being kept back. It is what lets a backend
/// whose integers are signed store an id as an integer rather than as bytes,
/// which is most of them. Without the promise, SQLite has to hold every id and
/// every `Ref` as an eight-byte blob, because one id at or above the limit
/// would bit-cast negative and sort first, and row order is the one thing about
/// a read that is not a hint. ADR 0014.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ObjectId(pub u64);

impl ObjectId {
    /// The first value an id may not take.
    pub const LIMIT: u64 = 1 << 63;

    /// Whether this one keeps the promise the type makes.
    pub const fn is_valid(self) -> bool {
        self.0 < Self::LIMIT
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// How a step names the object an index row points at.
///
/// A literal id, or the object of the row a `for_each` template is being
/// instantiated for. Keeping this its own enum is what stops an object slot
/// from being filled with an encoded email address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ObjectRef {
    /// A literal id, which is every case outside a template.
    Id(ObjectId),
    /// The object of the row a template is being instantiated for, which is
    /// why it is only writable inside a `for_each` body.
    Row,
}
