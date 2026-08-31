//! What a read produced, and how it gets its type back.
//!
//! A [`ReadValue`] is the untyped answer to a [`crate::query::plan::ReadPlan`].
//! [`FromReadValue`] is what a `Query<T>` uses to hand it back as `T`.

use crate::model::row::Row;
use crate::model::storage::FromStorage;
use crate::outcome::error::StorageError;

/// What a read produced, before it is given back its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadValue {
    /// How many rows a range held.
    Count(u64),
    /// One object's bytes, or `None` if it was not there.
    Object(Option<Vec<u8>>),
    /// Rows in index order, with no objects asked for.
    Rows(Vec<Row>),
    /// Rows with the object each one names, `None` where the mode was lenient
    /// and the object was gone.
    RowsWithObjects(Vec<(Row, Option<Vec<u8>>)>),
    /// Both halves came from one view of the store, so they agree with each
    /// other.
    Pair(Box<ReadValue>, Box<ReadValue>),
}

/// How a [`ReadValue`] becomes the type the query promised.
///
/// A mismatch is [`StorageError::ReadMismatch`] rather than a panic: the two
/// sides are built together and cannot disagree, but saying so with a type is
/// cheaper than proving it.
pub trait FromReadValue: Sized {
    /// Answers [`StorageError::ReadMismatch`] rather than panicking on a shape
    /// the query did not promise.
    fn from_read_value(value: ReadValue) -> Result<Self, StorageError>;
}

impl FromReadValue for u64 {
    fn from_read_value(value: ReadValue) -> Result<Self, StorageError> {
        match value {
            ReadValue::Count(count) => Ok(count),
            _ => Err(StorageError::ReadMismatch),
        }
    }
}

impl<T: FromStorage> FromReadValue for Option<T> {
    fn from_read_value(value: ReadValue) -> Result<Self, StorageError> {
        match value {
            ReadValue::Object(object) => object.as_deref().map(decode).transpose(),
            _ => Err(StorageError::ReadMismatch),
        }
    }
}

impl FromReadValue for Vec<Row> {
    fn from_read_value(value: ReadValue) -> Result<Self, StorageError> {
        match value {
            ReadValue::Rows(rows) => Ok(rows),
            _ => Err(StorageError::ReadMismatch),
        }
    }
}

impl<T: FromStorage> FromReadValue for Vec<(Row, T)> {
    fn from_read_value(value: ReadValue) -> Result<Self, StorageError> {
        match value {
            ReadValue::RowsWithObjects(rows) => rows
                .into_iter()
                .map(|(row, object)| match object {
                    Some(bytes) => Ok((row, decode(&bytes)?)),
                    None => Err(StorageError::ReadMismatch),
                })
                .collect(),
            _ => Err(StorageError::ReadMismatch),
        }
    }
}

impl<T: FromStorage> FromReadValue for Vec<(Row, Option<T>)> {
    fn from_read_value(value: ReadValue) -> Result<Self, StorageError> {
        match value {
            ReadValue::RowsWithObjects(rows) => rows
                .into_iter()
                .map(|(row, object)| Ok((row, object.as_deref().map(decode).transpose()?)))
                .collect(),
            _ => Err(StorageError::ReadMismatch),
        }
    }
}

/// An object's bytes are the caller's to interpret, so a failure here names
/// the codec rather than the store.
fn decode<T: FromStorage>(bytes: &[u8]) -> Result<T, StorageError> {
    T::from_storage(bytes).map_err(StorageError::Decode)
}

impl<A: FromReadValue, B: FromReadValue> FromReadValue for (A, B) {
    fn from_read_value(value: ReadValue) -> Result<Self, StorageError> {
        match value {
            ReadValue::Pair(left, right) => {
                Ok((A::from_read_value(*left)?, B::from_read_value(*right)?))
            }
            _ => Err(StorageError::ReadMismatch),
        }
    }
}
