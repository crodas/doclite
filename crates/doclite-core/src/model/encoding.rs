//! Values as bytes whose `memcmp` order is the value's own order.

use crate::model::index::ColumnType;
use crate::model::object::ObjectId;

/// Bytes that sort the way the value does.
///
/// An index compares its columns byte by byte and never interprets them, so
/// every ordering question is settled by an implementation of this rather than
/// by an engine. Columns of a compound key stay separate byte strings, so
/// nothing needs escaping to keep one column from bleeding into the next.
///
/// There is deliberately no `Ord` supertrait: a type may be encodable without
/// being comparable in Rust, and requiring both would exclude it for no gain.
///
/// A pointer-width integer is not encodable, because the bytes it produces
/// depend on the target that produced them:
///
/// ```compile_fail
/// use doclite_core::ToOrdBytes;
///
/// let key = 1usize.to_ord_bytes();
/// ```
///
/// The same key at a width every target agrees on:
///
/// ```
/// use doclite_core::ToOrdBytes;
///
/// let key = 1u64.to_ord_bytes();
/// assert_eq!(key, vec![0, 0, 0, 0, 0, 0, 0, 1]);
/// ```
pub trait ToOrdBytes {
    /// What this value is, for an engine that can lay a column out as more than
    /// bytes. It defaults to [`ColumnType::Bytes`], so a type nobody has
    /// described is stored as the bytes it already encodes to rather than
    /// refused.
    ///
    /// `usize` and `isize` are not columns at all. They encode at native
    /// width, so the same key would be a different number of bytes on a
    /// different target and a file written on one would misorder on the other.
    /// A caller naming one gets a compile error rather than a store that works
    /// until it moves; `u64` and `i64` are what to say instead.
    const COLUMN_TYPE: ColumnType = ColumnType::Bytes;

    /// The bytes an index compares. Two values compare the way their encodings
    /// do, and nothing else is promised about them.
    fn to_ord_bytes(&self) -> Vec<u8>;
}

macro_rules! unsigned {
    ($($t:ty => $c:ident),+ $(,)?) => {
        $(impl ToOrdBytes for $t {
            const COLUMN_TYPE: ColumnType = ColumnType::$c;

            fn to_ord_bytes(&self) -> Vec<u8> {
                self.to_be_bytes().to_vec()
            }
        })+
    };
}

/// Signed integers sort correctly once the sign bit is flipped, which turns
/// two's complement into an unsigned ordering.
macro_rules! signed {
    ($($t:ty => $u:ty, $c:ident),+ $(,)?) => {
        $(impl ToOrdBytes for $t {
            const COLUMN_TYPE: ColumnType = ColumnType::$c;

            fn to_ord_bytes(&self) -> Vec<u8> {
                let flipped = (*self as $u) ^ (1 << (<$u>::BITS - 1));
                flipped.to_be_bytes().to_vec()
            }
        })+
    };
}

unsigned!(u8 => U8, u16 => U16, u32 => U32, u64 => U64, u128 => U128);
signed!(
    i8 => u8, I8,
    i16 => u16, I16,
    i32 => u32, I32,
    i64 => u64, I64,
    i128 => u128, I128,
);

impl ToOrdBytes for bool {
    const COLUMN_TYPE: ColumnType = ColumnType::Bool;

    fn to_ord_bytes(&self) -> Vec<u8> {
        vec![u8::from(*self)]
    }
}

impl ToOrdBytes for char {
    const COLUMN_TYPE: ColumnType = ColumnType::Char;

    fn to_ord_bytes(&self) -> Vec<u8> {
        u32::from(*self).to_ord_bytes()
    }
}

impl ToOrdBytes for str {
    const COLUMN_TYPE: ColumnType = ColumnType::Str;

    fn to_ord_bytes(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl ToOrdBytes for String {
    const COLUMN_TYPE: ColumnType = ColumnType::Str;

    fn to_ord_bytes(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl ToOrdBytes for [u8] {
    fn to_ord_bytes(&self) -> Vec<u8> {
        self.to_vec()
    }
}

impl ToOrdBytes for Vec<u8> {
    fn to_ord_bytes(&self) -> Vec<u8> {
        self.clone()
    }
}

/// Signed, because [`ObjectId`] reserves its top bit and so always fits one.
///
/// The declared type and the bytes have to agree: an engine decodes a column
/// according to what the schema said it was, so declaring `I64` and encoding as
/// `u64` would read every id back off by a sign bit. Going through `i64` is
/// what keeps the two halves one decision, and it costs nothing in order,
/// because flipping the sign bit of a non-negative value preserves exactly the
/// ordering the unsigned encoding gave.
impl ToOrdBytes for ObjectId {
    const COLUMN_TYPE: ColumnType = ColumnType::I64;

    fn to_ord_bytes(&self) -> Vec<u8> {
        (self.0 as i64).to_ord_bytes()
    }
}

/// `None` leads with a zero byte and `Some` with a one, so an absent value
/// sorts before every present one and neither can be mistaken for the other.
impl<T: ToOrdBytes> ToOrdBytes for Option<T> {
    const COLUMN_TYPE: ColumnType = ColumnType::Nullable(&T::COLUMN_TYPE);

    fn to_ord_bytes(&self) -> Vec<u8> {
        match self {
            None => vec![0],
            Some(value) => {
                let mut bytes = vec![1];
                bytes.extend(value.to_ord_bytes());
                bytes
            }
        }
    }
}

impl<T: ToOrdBytes + ?Sized> ToOrdBytes for &T {
    const COLUMN_TYPE: ColumnType = T::COLUMN_TYPE;

    fn to_ord_bytes(&self) -> Vec<u8> {
        (*self).to_ord_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted<T: ToOrdBytes>(values: &[T]) -> bool {
        values
            .windows(2)
            .all(|w| w[0].to_ord_bytes() < w[1].to_ord_bytes())
    }

    #[test]
    fn unsigned_keeps_its_order() {
        assert!(sorted(&[0u32, 1, 255, 256, u32::MAX]));
    }

    #[test]
    fn signed_keeps_its_order_across_zero() {
        assert!(sorted(&[i64::MIN, -256, -1, 0, 1, 256, i64::MAX]));
    }

    #[test]
    fn strings_keep_their_order() {
        assert!(sorted(&["", "a", "ab", "b"]));
    }

    #[test]
    fn bool_orders_false_before_true() {
        assert!(sorted(&[false, true]));
    }

    #[test]
    fn none_sorts_before_every_some() {
        assert!(sorted(&[None, Some(0u32), Some(1), Some(u32::MAX)]));
    }

    /// A column type is a claim about the bytes beside it. An integer that says
    /// it is 32 bits and writes eight bytes would have a store declaring the
    /// wrong column.
    fn as_wide_as_it_claims<T: ToOrdBytes>(value: T) -> bool {
        let bits = match T::COLUMN_TYPE {
            ColumnType::U8 | ColumnType::I8 => 8,
            ColumnType::U16 | ColumnType::I16 => 16,
            ColumnType::U32 | ColumnType::I32 | ColumnType::Char => 32,
            ColumnType::U64 | ColumnType::I64 => 64,
            ColumnType::U128 | ColumnType::I128 => 128,
            _ => return true,
        };
        value.to_ord_bytes().len() * 8 == bits
    }

    #[test]
    fn every_fixed_width_column_writes_the_width_it_names() {
        assert!(as_wide_as_it_claims(0u8));
        assert!(as_wide_as_it_claims(0u16));
        assert!(as_wide_as_it_claims(0u32));
        assert!(as_wide_as_it_claims(0u64));
        assert!(as_wide_as_it_claims(0u128));
        assert!(as_wide_as_it_claims(0i8));
        assert!(as_wide_as_it_claims(0i16));
        assert!(as_wide_as_it_claims(0i32));
        assert!(as_wide_as_it_claims(0i64));
        assert!(as_wide_as_it_claims(0i128));
        assert!(as_wide_as_it_claims('a'));
        assert!(as_wide_as_it_claims(ObjectId(1)));
    }

    /// A type nobody has described falls back rather than being refused, which
    /// is what keeps the column type off a caller's list of things to learn.
    #[test]
    fn a_type_that_says_nothing_is_bytes() {
        assert_eq!(<Vec<u8> as ToOrdBytes>::COLUMN_TYPE, ColumnType::Bytes);
    }

    #[test]
    fn an_optional_column_is_the_column_it_wraps_made_nullable() {
        assert_eq!(
            <Option<u32> as ToOrdBytes>::COLUMN_TYPE,
            ColumnType::Nullable(&ColumnType::U32)
        );
        assert_eq!(<&str as ToOrdBytes>::COLUMN_TYPE, ColumnType::Str);
    }
}
