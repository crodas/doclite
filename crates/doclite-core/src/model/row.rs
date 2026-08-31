//! What a scan hands back.
//!
//! A row is the pair an index stores: the columns, and the object they name.
//! What a `for_each` template is handed instead is a [`crate::RowRef`],
//! which is a different thing and lives with the combinator that produces it.

use crate::model::object::ObjectId;

/// One row of one index: its columns, and the object it names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Row {
    /// The columns, as the writer encoded them.
    pub key: RowKey,
    /// The object those columns name.
    pub object: ObjectId,
}

impl Row {
    /// Takes the columns already encoded, since a row comes back from an engine
    /// that never interpreted them.
    pub fn new(columns: Vec<Vec<u8>>, object: ObjectId) -> Self {
        Self {
            key: RowKey(columns),
            object,
        }
    }
}

/// A row's columns, in the index's own order.
///
/// The bytes are whatever the writer encoded, so reading one back means
/// knowing what it was. That is the whole cost of the engine never
/// interpreting a column.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct RowKey(Vec<Vec<u8>>);

impl RowKey {
    /// `None` past the end, because an index's arity is not a fact this type
    /// carries.
    pub fn component(&self, at: usize) -> Option<&[u8]> {
        self.0.get(at).map(Vec::as_slice)
    }

    /// Every component, in the index's own order.
    pub fn components(&self) -> &[Vec<u8>] {
        &self.0
    }

    /// How many components the row has, which is the arity of the index it came
    /// from.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// A row of the arity-zero index, which is the only way this is true.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
