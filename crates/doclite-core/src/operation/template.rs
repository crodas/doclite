//! What a `for_each` body is handed, and what it may do with it.
//!
//! A template body is called once, before any row exists, so what it names are
//! slots rather than values. The types here are the whole of that surface: they
//! fit a key and an object slot, and nothing else.

use crate::model::column::{Column, IntoColumn, IntoObject};
use crate::model::object::ObjectRef;

/// A row that does not exist yet, which a `for_each` body builds against.
///
/// Its methods hand back [`Cell`]s: slots the engine fills once per row. A body
/// can put one in a key and nothing else, which is what keeps a template a
/// template.
#[derive(Debug, Clone, Copy)]
pub struct RowRef;

impl RowRef {
    /// The column at this position on the row.
    pub fn key(&self, at: usize) -> Cell {
        Cell(at)
    }

    /// The object the row names.
    pub fn doc(&self) -> ObjectCell {
        ObjectCell
    }
}

/// One column of the row a `for_each` body is being built against.
///
/// It carries no `PartialEq` and no `Ord` on purpose. A template is built once,
/// before any row exists, so there is nothing here to compare against; making
/// the comparison unwritable is what keeps a body constructing rather than
/// branching.
#[derive(Debug, Clone, Copy)]
pub struct Cell(pub(crate) usize);

/// The object that row names.
///
/// Separate from [`Cell`] so it is the only thing that fits an object slot: a
/// key column cannot be passed where an id belongs.
#[derive(Debug, Clone, Copy)]
pub struct ObjectCell;

impl IntoColumn for Cell {
    fn into_column(self) -> Column {
        Column::RowColumn(self.0)
    }
}

impl IntoColumn for ObjectCell {
    fn into_column(self) -> Column {
        Column::RowObject
    }
}

impl IntoObject for ObjectCell {
    fn into_object(self) -> ObjectRef {
        ObjectRef::Row
    }
}
