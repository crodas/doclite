//! How a column and an object id are held, and why each one is legal.
//!
//! Every mapping here owes two proofs: SQLite's ordering for that storage class
//! is the `memcmp` order of the bytes `ToOrdBytes` produced, and the bytes come
//! back byte-identical, because an assertion compares them for equality. A type
//! that cannot pay both is stored as a blob, which always can.

use doclite_core::{ColumnType, ObjectId, StorageError};
use rusqlite::types::{ToSqlOutput, Value, ValueRef};

use crate::catalog::{DOCUMENTS, JSON_DOCUMENTS};

/// Which table an object's bytes belong in, and how they are bound once there.
///
/// Two tables rather than two storage classes in one column, so neither `body`
/// ever holds both and the mixed-class rule this module keeps for key columns is
/// not bent for objects either. What it buys is a record written by
/// `#[storage(json)]` that `sqlite3` prints as JSON and `json_extract` reads,
/// sitting in a table that holds nothing else.
///
/// The split is decided by the bytes and not by the codec: an engine is never
/// told which record chose what, so "is this text" is the only question it can
/// ask. Nothing is refused either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Body {
    /// Valid UTF-8, bound as a text value so it reads as itself.
    Text,
    /// Everything else, bound as a blob.
    Blob,
}

impl Body {
    /// Which of the two an object's bytes are.
    pub(crate) fn of(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(_) => Self::Text,
            Err(_) => Self::Blob,
        }
    }

    /// The table that holds this kind.
    pub(crate) fn table(self) -> &'static str {
        match self {
            Self::Text => JSON_DOCUMENTS,
            Self::Blob => DOCUMENTS,
        }
    }

    /// The bytes as SQLite should hold them.
    ///
    /// Bound explicitly rather than left to affinity, which converts nothing:
    /// bytes handed to a `TEXT` column as a blob stay a blob.
    pub(crate) fn encode(bytes: &[u8]) -> ToSqlOutput<'_> {
        match Self::of(bytes) {
            Self::Text => ToSqlOutput::Borrowed(ValueRef::Text(bytes)),
            Self::Blob => ToSqlOutput::Borrowed(ValueRef::Blob(bytes)),
        }
    }

    /// The bytes back, from whichever table they came out of.
    ///
    /// `ValueRef::as_bytes` takes text and blob alike, where a `Vec<u8>` column
    /// read goes through `as_blob` and would refuse everything in the JSON
    /// table.
    pub(crate) fn decode(value: ValueRef<'_>) -> Result<Vec<u8>, StorageError> {
        value.as_bytes().map(<[u8]>::to_vec).map_err(|_| {
            StorageError::Engine(format!("an object body came back as {value:?}, not bytes"))
        })
    }
}

/// How wide an object id is on disk.
const ID_BYTES: usize = 8;

/// What a key column is stored as.
///
/// `Integer` carries the width it decodes, because the bytes have to be checked
/// against it before they can be trusted and re-encoded after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Storage {
    Integer { bytes: usize, signed: bool },
    Text,
    Blob,
}

impl Storage {
    /// What SQLite is told the column holds.
    ///
    /// The declaration is an affinity rather than a constraint, so it is a
    /// statement of intent that this module then has to keep.
    pub(crate) fn declared(self) -> &'static str {
        match self {
            Self::Integer { .. } => "INTEGER",
            Self::Text => "TEXT",
            Self::Blob => "BLOB",
        }
    }

    /// Which storage a declared column type earns.
    ///
    /// A type wider than SQLite's signed 64-bit integer stays bytes, because
    /// there is nothing to widen it into. `Nullable` takes the storage of what
    /// it wraps only when the tag byte cannot be confused for the body, which
    /// is never: the tag makes it variable-width, so it stays bytes.
    pub(crate) fn of(column: &ColumnType) -> Self {
        match column {
            ColumnType::Bool => Self::Integer {
                bytes: 1,
                signed: false,
            },
            ColumnType::U8 => Self::Integer {
                bytes: 1,
                signed: false,
            },
            ColumnType::U16 => Self::Integer {
                bytes: 2,
                signed: false,
            },
            ColumnType::U32 => Self::Integer {
                bytes: 4,
                signed: false,
            },
            ColumnType::Char => Self::Integer {
                bytes: 4,
                signed: false,
            },
            ColumnType::I8 => Self::Integer {
                bytes: 1,
                signed: true,
            },
            ColumnType::I16 => Self::Integer {
                bytes: 2,
                signed: true,
            },
            ColumnType::I32 => Self::Integer {
                bytes: 4,
                signed: true,
            },
            ColumnType::I64 => Self::Integer {
                bytes: 8,
                signed: true,
            },
            ColumnType::Str => Self::Text,
            _ => Self::Blob,
        }
    }

    /// The bytes as SQLite should hold them.
    ///
    /// Bytes of the wrong width for the declared column are the engine's
    /// problem rather than the caller's: the declaration promised a storage
    /// class, and a column that silently held two of them would not be ordered
    /// by the rule this design needs.
    pub(crate) fn encode(self, bytes: &[u8]) -> Result<ToSqlOutput<'static>, StorageError> {
        match self {
            Self::Integer {
                bytes: width,
                signed,
            } => {
                let raw = Self::fixed(bytes, width)?;
                Ok(ToSqlOutput::Owned(Value::Integer(Self::number(
                    &raw, signed,
                ))))
            }
            Self::Text => {
                let text = String::from_utf8(bytes.to_vec()).map_err(|error| {
                    StorageError::Engine(format!("a text column that is not utf-8: {error}"))
                })?;
                Ok(ToSqlOutput::Owned(Value::Text(text)))
            }
            Self::Blob => Ok(ToSqlOutput::Owned(Value::Blob(bytes.to_vec()))),
        }
    }

    /// The bytes back, exactly as they were written.
    pub(crate) fn decode(self, value: ValueRef<'_>) -> Result<Vec<u8>, StorageError> {
        match (self, value) {
            (Self::Integer { bytes, signed }, ValueRef::Integer(number)) => {
                Self::unnumber(number, bytes, signed)
            }
            (Self::Text, ValueRef::Text(text)) => Ok(text.to_vec()),
            (Self::Blob, ValueRef::Blob(blob)) => Ok(blob.to_vec()),
            (storage, other) => Err(StorageError::Engine(format!(
                "a {} column came back as {other:?}",
                storage.declared()
            ))),
        }
    }
}

/// An object id as this engine holds it: one SQLite integer.
///
/// An integer rather than eight bytes because [`ObjectId`] reserves its top
/// bit, so every id fits a signed sixty-four and none of them bit-casts
/// negative. That is what keeps `ORDER BY .., object` the order the executor
/// asked for, which is the one thing about a read that is not a hint. Without
/// the reservation this would have to be a blob. ADR 0014.
///
/// A type rather than two functions, so the choice has somewhere to live rather
/// than being agreed on in two places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ObjectKey(i64);

impl ObjectKey {
    /// The cast cannot wrap: `Check` refuses an out-of-range id before any
    /// operation naming one reaches an engine.
    pub(crate) fn of(id: ObjectId) -> Self {
        Self(id.0 as i64)
    }

    /// The id this integer names, refusing a negative nothing here ever wrote.
    ///
    /// Only reachable if something other than this engine wrote the row, and a
    /// wrong answer about identity is worse than a refusal.
    pub(crate) fn read(value: i64) -> Result<Self, StorageError> {
        if value < 0 {
            return Err(StorageError::Engine(format!(
                "an object id of {value}, which is negative and so was not written here"
            )));
        }
        Ok(Self(value))
    }

    pub(crate) fn id(self) -> ObjectId {
        ObjectId(self.0 as u64)
    }

    pub(crate) fn value(self) -> i64 {
        self.0
    }
}

impl Storage {
    /// Where a column of this width starts inside eight bytes.
    ///
    /// `of` builds no integer column wider than eight, but that is a fact about
    /// `of`. Said here it is a fact about the subtraction, which is the one
    /// place it has to hold.
    fn offset(width: usize) -> Result<usize, StorageError> {
        ID_BYTES.checked_sub(width).ok_or_else(|| {
            StorageError::Engine(format!(
                "a column declared {width} bytes wide, wider than the {ID_BYTES} an integer holds"
            ))
        })
    }

    /// The bytes widened to eight, refusing anything that is not the width the
    /// column declared.
    fn fixed(bytes: &[u8], width: usize) -> Result<[u8; ID_BYTES], StorageError> {
        let offset = Self::offset(width)?;
        if bytes.len() != width {
            return Err(StorageError::Engine(format!(
                "a column declared {width} bytes wide arrived with {}",
                bytes.len()
            )));
        }
        let mut raw = [0u8; ID_BYTES];
        raw[offset..].copy_from_slice(bytes);
        Ok(raw)
    }

    /// The number those bytes encode, in the order they encode it.
    ///
    /// `ToOrdBytes` writes a signed value with its sign bit flipped, so undoing
    /// the flip is what turns the stored order back into the value's own.
    fn number(raw: &[u8; ID_BYTES], signed: bool) -> i64 {
        let value = u64::from_be_bytes(*raw);
        if signed {
            (value ^ (1 << (ID_BYTES * 8 - 1))) as i64
        } else {
            value as i64
        }
    }

    /// The bytes back at the width the column declared.
    fn unnumber(number: i64, width: usize, signed: bool) -> Result<Vec<u8>, StorageError> {
        let offset = Self::offset(width)?;
        let value = if signed {
            (number as u64) ^ (1 << (ID_BYTES * 8 - 1))
        } else {
            number as u64
        };
        Ok(value.to_be_bytes()[offset..].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(column: ColumnType, bytes: &[u8]) {
        let storage = Storage::of(&column);
        let encoded = storage.encode(bytes).expect("the bytes encode");
        let value = match &encoded {
            ToSqlOutput::Owned(Value::Integer(n)) => ValueRef::Integer(*n),
            ToSqlOutput::Owned(Value::Text(t)) => ValueRef::Text(t.as_bytes()),
            ToSqlOutput::Owned(Value::Blob(b)) => ValueRef::Blob(b),
            _ => panic!("an unexpected encoding"),
        };
        assert_eq!(storage.decode(value).expect("the value decodes"), bytes);
    }

    /// `of` builds nothing wider than eight, so this reaches past it and
    /// builds one by hand: the refusal has to belong to the subtraction rather
    /// than to the function that happens to feed it.
    #[test]
    fn a_column_wider_than_an_integer_is_refused_rather_than_subtracted() {
        let storage = Storage::Integer {
            bytes: ID_BYTES + 1,
            signed: false,
        };

        let Err(StorageError::Engine(refused)) = storage.encode(&[0; ID_BYTES + 1]) else {
            panic!("a column wider than eight bytes has nowhere to go");
        };
        assert!(refused.contains("wider than"), "it says so: {refused}");

        let Err(StorageError::Engine(refused)) = storage.decode(ValueRef::Integer(0)) else {
            panic!("and it has nowhere to come back from");
        };
        assert!(refused.contains("wider than"), "it says so: {refused}");
    }

    #[test]
    fn every_integer_width_comes_back_as_it_went_in() {
        roundtrip(ColumnType::U8, &[0xff]);
        roundtrip(ColumnType::U16, &[0x00, 0x01]);
        roundtrip(ColumnType::U32, &[0xff, 0xff, 0xff, 0xff]);
        roundtrip(ColumnType::I8, &[0x00]);
        roundtrip(ColumnType::I64, &[0x80, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_signed_column_orders_across_zero() {
        let storage = Storage::of(&ColumnType::I32);
        let below = storage
            .encode(&(-1i32 as u32 ^ (1 << 31)).to_be_bytes())
            .expect("encodes");
        let above = storage
            .encode(&(1i32 as u32 ^ (1 << 31)).to_be_bytes())
            .expect("encodes");
        let (ToSqlOutput::Owned(Value::Integer(low)), ToSqlOutput::Owned(Value::Integer(high))) =
            (below, above)
        else {
            panic!("a signed column is an integer");
        };
        assert!(low < high, "-1 sorts below 1");
    }

    #[test]
    fn a_wide_integer_stays_bytes() {
        assert_eq!(Storage::of(&ColumnType::U64), Storage::Blob);
        assert_eq!(Storage::of(&ColumnType::U128), Storage::Blob);
        assert_eq!(Storage::of(&ColumnType::I128), Storage::Blob);
    }

    /// The tag byte makes an `Option` variable-width, so it cannot be a fixed
    /// column whatever it wraps.
    #[test]
    fn a_nullable_column_stays_bytes() {
        assert_eq!(
            Storage::of(&ColumnType::Nullable(&ColumnType::U32)),
            Storage::Blob
        );
        assert_eq!(
            Storage::of(&ColumnType::Nullable(&ColumnType::Str)),
            Storage::Blob
        );
    }

    #[test]
    fn a_column_of_the_wrong_width_is_refused_rather_than_widened() {
        assert!(Storage::of(&ColumnType::U32).encode(&[1, 2]).is_err());
    }

    #[test]
    fn an_object_id_survives_every_value_it_may_take() {
        for id in [0, 1, ObjectId::LIMIT / 2, ObjectId::LIMIT - 1] {
            let held = ObjectKey::of(ObjectId(id));
            assert_eq!(ObjectKey::read(held.value()), Ok(held));
            assert_eq!(held.id(), ObjectId(id));
        }
    }

    /// What the reserved bit buys: every id an operation may name is a
    /// non-negative integer, so SQLite's ordering is the executor's, and index
    /// order is not a hint.
    #[test]
    fn a_high_object_id_still_sorts_above_a_low_one() {
        assert!(ObjectKey::of(ObjectId(1)) < ObjectKey::of(ObjectId(ObjectId::LIMIT - 1)));
        assert!(ObjectKey::of(ObjectId(ObjectId::LIMIT - 1)).value() > 0);
    }

    /// Only reachable if something else wrote the row, and identity is not
    /// something to guess at.
    #[test]
    fn a_negative_object_id_is_refused_rather_than_reinterpreted() {
        assert!(ObjectKey::read(-1).is_err());
    }

    /// Two storage classes in one column would order wrongly, since SQLite puts
    /// every text value below every blob. A blob column holding text is that,
    /// so it is refused rather than read.
    #[test]
    fn a_blob_column_refuses_a_text_value() {
        assert!(Storage::Blob.decode(ValueRef::Text(b"not a blob")).is_err());
    }
}
