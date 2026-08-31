//! A key, and how many components it has.

use crate::model::column::Column;

/// A key whose component count is part of its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key<const N: usize> {
    columns: [Column; N],
}

impl<const N: usize> Key<N> {
    /// Takes an array rather than a slice, so the width is settled where the key
    /// is built rather than where it is filed.
    pub fn from_columns(columns: [Column; N]) -> Self {
        Self { columns }
    }

    /// The components, in the order the index declares them.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The components as a `Vec`, for a caller that is done with the width.
    pub fn into_columns(self) -> Vec<Column> {
        self.columns.into()
    }
}

/// Builds a [`Key`] from its components, encoding each one.
///
/// Every component goes through [`IntoColumn`](crate::IntoColumn), so the same
/// macro writes a key of ordinary values and a key of row slots inside a
/// `for_each` body.
#[macro_export]
macro_rules! key {
    ($($component:expr),+ $(,)?) => {
        $crate::Key::from_columns([
            $($crate::IntoColumn::into_column($component)),+
        ])
    };
}
