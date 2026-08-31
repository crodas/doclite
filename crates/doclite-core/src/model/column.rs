//! One column of a key: bytes, or where to find them on a row not yet read.

use crate::model::encoding::ToOrdBytes;
use crate::model::object::{ObjectId, ObjectRef};
use crate::outcome::error::BuildError;

/// Bytes, or where to find them on a row.
///
/// Everything a step holds is a `Column`. Outside a `for_each` template every
/// one of them is [`Column::Literal`]; the other two are slots, written with
/// [`Cell`](crate::Cell) and [`ObjectCell`](crate::ObjectCell), and refused anywhere
/// a row will never exist.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Column {
    /// Bytes already encoded, which is every column outside a template.
    Literal(Vec<u8>),
    /// The column at this position on the row a template is instantiated for.
    RowColumn(usize),
    /// The object that row names, encoded as a column.
    RowObject,
}

impl Column {
    /// Goes through [`ToOrdBytes`], so the column sorts the way the value does.
    pub fn encode<T: ToOrdBytes + ?Sized>(value: &T) -> Self {
        Self::Literal(value.to_ord_bytes())
    }

    /// Whether this is a slot rather than bytes, which is what decides where it
    /// may appear.
    pub fn is_symbolic(&self) -> bool {
        !matches!(self, Self::Literal(_))
    }
}

/// Refuses a row slot where no row will ever exist.
///
/// The build-time half of what [`bind::Bind`](crate::executor::bind::Bind) does
/// at run time: one rejects the slot, the other fills it, and a change to
/// either without the other is a bug.
pub fn symbol_allowed(column: &Column, in_template: bool) -> Result<(), BuildError> {
    if column.is_symbolic() && !in_template {
        return Err(BuildError::RowOutsideTemplate);
    }
    Ok(())
}

/// What a key component may be written as.
///
/// There is no blanket impl over [`ToOrdBytes`]: it would overlap
/// [`Cell`](crate::Cell), and the overlap is not something coherence can
/// rule out. A type outside this list reaches a key through [`Column::encode`].
pub trait IntoColumn {
    /// Consumes the value, because a column owns its bytes once encoded.
    fn into_column(self) -> Column;
}

impl IntoColumn for Column {
    fn into_column(self) -> Column {
        self
    }
}

impl IntoColumn for &str {
    fn into_column(self) -> Column {
        Column::encode(self)
    }
}

impl IntoColumn for &[u8] {
    fn into_column(self) -> Column {
        Column::encode(self)
    }
}

macro_rules! encodable {
    ($($t:ty),+ $(,)?) => {
        $(
            impl IntoColumn for $t {
                fn into_column(self) -> Column {
                    Column::encode(&self)
                }
            }

            impl IntoColumn for &$t {
                fn into_column(self) -> Column {
                    Column::encode(self)
                }
            }
        )+
    };
}

encodable!(
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    bool,
    char,
    String,
    Vec<u8>,
    ObjectId,
);

/// An optional value is a column like any other, encoded with the tag byte
/// [`ToOrdBytes`] gives it, so an index may hold a field that is sometimes
/// absent.
impl<T: ToOrdBytes> IntoColumn for Option<T> {
    fn into_column(self) -> Column {
        Column::encode(&self)
    }
}

/// What may name an object where a step expects one.
///
/// Narrower than [`IntoColumn`] so an object id slot cannot be filled with an
/// email address.
pub trait IntoObject {
    /// Narrow on purpose: the only conversions are the ones that really name an
    /// object.
    fn into_object(self) -> ObjectRef;
}

impl IntoObject for ObjectId {
    fn into_object(self) -> ObjectRef {
        ObjectRef::Id(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal(value: &str) -> Column {
        Column::Literal(value.as_bytes().to_vec())
    }

    #[test]
    fn an_absent_value_is_a_column_that_sorts_first() {
        let absent = Option::<u32>::None.into_column();
        let present = Some(0u32).into_column();
        assert!(!absent.is_symbolic());
        assert!(matches!((&absent, &present),
            (Column::Literal(a), Column::Literal(b)) if a < b));
    }

    #[test]
    fn a_slot_is_only_writable_inside_a_template() {
        assert_eq!(symbol_allowed(&literal("a"), false), Ok(()));
        assert_eq!(symbol_allowed(&Column::RowColumn(0), true), Ok(()));
        assert_eq!(
            symbol_allowed(&Column::RowObject, false),
            Err(BuildError::RowOutsideTemplate)
        );
    }
}
