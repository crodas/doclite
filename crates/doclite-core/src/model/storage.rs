//! An object as bytes, and back.

use crate::outcome::error::CodecError;

/// How a value becomes the bytes an object is stored as.
///
/// The engine never interprets an object, so the encoding is settled here and
/// the store learns nothing from it. This is the object half of what
/// [`ToOrdBytes`](crate::ToOrdBytes) is for keys, and the two answer different
/// questions: a key's bytes have to sort, an object's only have to decode.
pub trait ToStorage {
    /// Whether these bytes are text, so a layer above may nest them inside its
    /// own encoding rather than framing them in binary.
    ///
    /// It is a promise about the bytes and not about the type: an implementation
    /// that says `true` and writes something else has broken the layer that
    /// believed it. `false` is the default, because a codec that has not thought
    /// about this is one whose bytes nothing should try to read.
    const TEXT: bool = false;

    /// Fallible, because a value that cannot encode is the caller's to hear
    /// about rather than a panic on the write path.
    fn to_storage(&self) -> Result<Vec<u8>, CodecError>;
}

/// How an object's bytes become a value again.
///
/// An object written today is read back years later against a type that has
/// since changed, and nothing rewrites it in between, so an implementation is
/// only as good as it is tolerant of a shape it did not write.
pub trait FromStorage: Sized {
    /// The read half of [`ToStorage::TEXT`], and it has to agree with it: a
    /// layer that framed a value one way decodes it the same way.
    const TEXT: bool = false;

    /// Given the object's bytes exactly as they were written, since nothing
    /// rewrites an object between the write and this call.
    fn from_storage(bytes: &[u8]) -> Result<Self, CodecError>;
}

impl ToStorage for Vec<u8> {
    fn to_storage(&self) -> Result<Vec<u8>, CodecError> {
        Ok(self.clone())
    }
}

impl ToStorage for [u8] {
    fn to_storage(&self) -> Result<Vec<u8>, CodecError> {
        Ok(self.to_vec())
    }
}

impl<T: ToStorage + ?Sized> ToStorage for &T {
    fn to_storage(&self) -> Result<Vec<u8>, CodecError> {
        (*self).to_storage()
    }
}

impl FromStorage for Vec<u8> {
    fn from_storage(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(bytes.to_vec())
    }
}

/// Implements [`ToStorage`] and [`FromStorage`] for a type that already derives
/// serde's `Serialize` and `Deserialize`, encoding it as CBOR.
///
/// One line per type rather than a blanket impl over `Serialize`, because a
/// blanket would overlap the byte impls above and would leave no room for a
/// type that wants an encoding of its own.
///
/// ```
/// # #[cfg(feature = "cbor")] {
/// use serde::{Deserialize, Serialize};
/// use doclite_core::{FromStorage, ToStorage};
///
/// #[derive(Debug, PartialEq, Serialize, Deserialize)]
/// struct User {
///     email: String,
/// }
///
/// doclite_core::serde_storage!(User);
///
/// let user = User { email: "a@b.c".into() };
/// let bytes = user.to_storage().expect("a struct of strings encodes");
/// assert_eq!(User::from_storage(&bytes), Ok(user));
/// # }
/// ```
#[cfg(feature = "cbor")]
#[macro_export]
macro_rules! serde_storage {
    ($($t:ty),+ $(,)?) => {
        $(
            impl $crate::ToStorage for $t {
                fn to_storage(&self) -> ::core::result::Result<Vec<u8>, $crate::CodecError> {
                    let mut bytes = Vec::new();
                    $crate::reexport::ciborium::into_writer(self, &mut bytes)
                        .map_err(|error| $crate::CodecError::encoding(error))?;
                    Ok(bytes)
                }
            }

            impl $crate::FromStorage for $t {
                fn from_storage(
                    bytes: &[u8],
                ) -> ::core::result::Result<Self, $crate::CodecError> {
                    $crate::reexport::ciborium::from_reader(bytes)
                        .map_err(|error| $crate::CodecError::decoding(error))
                }
            }
        )+
    };
}

/// Implements [`ToStorage`] and [`FromStorage`] for a type that already derives
/// serde's `Serialize` and `Deserialize`, encoding it as JSON.
///
/// The same shape as [`serde_storage!`] and chosen for the same reason: JSON is
/// self-describing, so an object written before its struct gained a field still
/// decodes afterwards. What it buys over CBOR is a store a person can read, and
/// what it costs is size. `#[derive(Document)]` writes this line for a record
/// marked `#[storage(json)]`.
///
/// Not for a record carrying bytes. `serde_json` encodes a `Vec<u8>` as an
/// array of numbers, which round-trips and is useless to read, which is why the
/// derive refuses the attribute on such a record rather than expanding to this.
///
/// ```
/// # #[cfg(feature = "json")] {
/// use serde::{Deserialize, Serialize};
/// use doclite_core::{FromStorage, ToStorage};
///
/// #[derive(Debug, PartialEq, Serialize, Deserialize)]
/// struct User {
///     email: String,
/// }
///
/// doclite_core::json_storage!(User);
///
/// let user = User { email: "a@b.c".into() };
/// let bytes = user.to_storage().expect("a struct of strings encodes");
/// assert_eq!(String::from_utf8_lossy(&bytes), r#"{"email":"a@b.c"}"#);
/// assert_eq!(User::from_storage(&bytes), Ok(user));
/// # }
/// ```
#[cfg(feature = "json")]
#[macro_export]
macro_rules! json_storage {
    ($($t:ty),+ $(,)?) => {
        $(
            impl $crate::ToStorage for $t {
                const TEXT: bool = true;

                fn to_storage(&self) -> ::core::result::Result<Vec<u8>, $crate::CodecError> {
                    $crate::reexport::serde_json::to_vec(self)
                        .map_err(|error| $crate::CodecError::encoding(error))
                }
            }

            impl $crate::FromStorage for $t {
                const TEXT: bool = true;

                fn from_storage(
                    bytes: &[u8],
                ) -> ::core::result::Result<Self, $crate::CodecError> {
                    $crate::reexport::serde_json::from_slice(bytes)
                        .map_err(|error| $crate::CodecError::decoding(error))
                }
            }
        )+
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_go_in_and_come_back_unchanged() {
        let object = b"a blob".to_vec();
        let stored = object.to_storage().expect("bytes are already bytes");
        assert_eq!(stored, object);
        assert_eq!(Vec::from_storage(&stored), Ok(object));
    }

    /// Taken by value, the way `put_object` takes it, so the reference impl
    /// is the one under test rather than an auto-deref.
    fn stored(value: impl ToStorage) -> Result<Vec<u8>, CodecError> {
        value.to_storage()
    }

    #[test]
    fn a_reference_encodes_as_what_it_points_at() {
        let object = b"a blob".to_vec();
        assert_eq!(stored(&object), stored(object.clone()));
    }

    /// The point of the JSON codec: the bytes are the document, so a store
    /// holding them can be read by something that is not this crate.
    #[cfg(feature = "json")]
    #[test]
    fn a_json_object_is_its_own_text() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct User {
            email: String,
            age: u8,
        }
        crate::json_storage!(User);

        let user = User {
            email: "a@b.c".into(),
            age: 24,
        };
        let bytes = user.to_storage().expect("a struct of scalars encodes");
        assert_eq!(
            std::str::from_utf8(&bytes),
            Ok(r#"{"email":"a@b.c","age":24}"#)
        );
        assert_eq!(User::from_storage(&bytes), Ok(user));
    }

    /// What ADR 0001 asks of a codec: an object written before its struct grew
    /// a field still decodes, because the encoding names what it holds.
    #[cfg(feature = "json")]
    #[test]
    fn a_json_object_survives_a_field_being_added() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Before {
            email: String,
        }
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct After {
            email: String,
            #[serde(default)]
            age: u8,
        }
        crate::json_storage!(Before);
        crate::json_storage!(After);

        let bytes = Before {
            email: "a@b.c".into(),
        }
        .to_storage()
        .expect("a struct of one string encodes");
        assert_eq!(
            After::from_storage(&bytes),
            Ok(After {
                email: "a@b.c".into(),
                age: 0,
            })
        );
    }
}
