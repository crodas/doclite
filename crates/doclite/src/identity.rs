//! What a stored object is, once this layer is the one naming it.
//!
//! `doclite-core` knows one identity: an `ObjectId` names bytes. A record is a
//! chain of those, so this layer needs two words for what used to be one. They
//! are separate types rather than aliases because the derive writes one into a
//! key and the other into the row's payload, and swapping them silently would
//! index the wrong thing.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use doclite_core::{Column, ColumnType, IntoColumn, IntoObject, ObjectId, ObjectRef, ToOrdBytes};

/// One revision: the object this particular version of a record was written as.
///
/// A new one is minted for every edit, so it is what an index row points at and
/// what `prev` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RevisionId(pub ObjectId);

/// A record, across every revision it ever has.
///
/// It is the `ObjectId` of the first revision and never changes afterwards, so
/// it is what a caller holds onto and what another record stores as a foreign
/// key. `Stored::id` hands this one out for that reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DocumentId(pub ObjectId);

impl RevisionId {
    /// The first revision of a record is also its identity.
    pub fn opens(self) -> DocumentId {
        DocumentId(self.0)
    }
}

impl fmt::Display for RevisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl ToOrdBytes for RevisionId {
    const COLUMN_TYPE: ColumnType = <ObjectId as ToOrdBytes>::COLUMN_TYPE;

    fn to_ord_bytes(&self) -> Vec<u8> {
        self.0.to_ord_bytes()
    }
}

impl ToOrdBytes for DocumentId {
    const COLUMN_TYPE: ColumnType = <ObjectId as ToOrdBytes>::COLUMN_TYPE;

    fn to_ord_bytes(&self) -> Vec<u8> {
        self.0.to_ord_bytes()
    }
}

impl IntoColumn for RevisionId {
    fn into_column(self) -> Column {
        Column::encode(&self)
    }
}

impl IntoColumn for DocumentId {
    fn into_column(self) -> Column {
        Column::encode(&self)
    }
}

/// An index row points at the revision, because that is the object holding the
/// bytes a scan has to decode.
impl IntoObject for RevisionId {
    fn into_object(self) -> ObjectRef {
        ObjectRef::Id(self.0)
    }
}

/// A reference to another record, by the identity that never moves.
///
/// On disk this is the id and nothing else: the same bytes a bare
/// [`DocumentId`] writes, because the type it points at is a fact about the
/// schema rather than about the row. What the parameter buys is that a
/// `Ref<Account>` cannot be followed as a `Deal`, and that a field says what it
/// points at without a comment.
///
/// Following one is a read. A reference names a record rather than a revision,
/// so what comes back is whichever revision is live when it is followed, not
/// whichever was live when it was written. See [`Connection::load`].
///
/// [`Connection::load`]: crate::Connection::load
#[derive(Serialize, Deserialize)]
#[serde(transparent, bound = "")]
pub struct Ref<T> {
    id: DocumentId,
    #[serde(skip)]
    target: PhantomData<fn() -> T>,
}

impl<T> Ref<T> {
    /// Names a document as a `T` without reading it, which is what a foreign
    /// key is.
    pub fn new(id: DocumentId) -> Self {
        Self {
            id,
            target: PhantomData,
        }
    }

    /// The record this names, with the type forgotten.
    pub fn id(self) -> DocumentId {
        self.id
    }
}

/// Derived impls would demand `T: Clone` and the rest, which is wrong: a
/// reference is a `u64` whatever it points at, and the parameter is only there
/// to say what following it yields.
impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Ref<T> {}

impl<T> PartialEq for Ref<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<T> Eq for Ref<T> {}

impl<T> PartialOrd for Ref<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Ref<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
}

impl<T> Hash for Ref<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<T> fmt::Debug for Ref<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ref({})", self.id)
    }
}

impl<T> fmt::Display for Ref<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.id)
    }
}

impl<T> From<DocumentId> for Ref<T> {
    fn from(id: DocumentId) -> Self {
        Self::new(id)
    }
}

impl<T> ToOrdBytes for Ref<T> {
    const COLUMN_TYPE: ColumnType = <DocumentId as ToOrdBytes>::COLUMN_TYPE;

    fn to_ord_bytes(&self) -> Vec<u8> {
        self.id.to_ord_bytes()
    }
}

impl<T> IntoColumn for Ref<T> {
    fn into_column(self) -> Column {
        Column::encode(&self)
    }
}

#[cfg(test)]
mod tests {
    use doclite_core::{FromStorage, ToStorage};
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Account;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Holder {
        account: Ref<Account>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Bare {
        account: DocumentId,
    }

    doclite_core::serde_storage!(Holder);
    doclite_core::serde_storage!(Bare);

    fn id(at: u64) -> DocumentId {
        DocumentId(ObjectId(at))
    }

    /// The parameter is a fact about the schema, not about the row, so it must
    /// not reach the bytes. A store written before a field was typed has to
    /// read back the same afterwards.
    #[test]
    fn a_reference_is_stored_as_the_bare_id() {
        let typed = Holder {
            account: Ref::<Account>::new(id(7)),
        }
        .to_storage()
        .expect("a holder encodes");
        let bare = Bare { account: id(7) }.to_storage().expect("bare encodes");

        assert_eq!(typed, bare, "a Ref writes what a DocumentId writes");
    }

    #[test]
    fn a_reference_reads_back_from_bytes_a_bare_id_wrote() {
        let bare = Bare { account: id(7) }.to_storage().expect("bare encodes");

        assert_eq!(
            Holder::from_storage(&bare),
            Ok(Holder {
                account: Ref::new(id(7))
            })
        );
    }

    #[test]
    fn a_reference_sorts_and_compares_as_the_id_it_holds() {
        let first: Ref<Account> = Ref::new(id(1));
        let second: Ref<Account> = Ref::new(id(2));

        assert!(first < second);
        assert_eq!(first, Ref::new(id(1)));
        assert_eq!(first.to_ord_bytes(), id(1).to_ord_bytes());
        assert_eq!(first.id(), id(1));
    }
}
