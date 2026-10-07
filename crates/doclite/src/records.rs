//! The record types a connection was opened over, and what each one can do
//! about an index whose rows are missing.
//!
//! `Connection::open` used to take `&[IndexSchema]`, which says which indexes
//! exist and nothing about how a row for one is derived. That is enough to
//! declare them and not enough to fill one: filling means decoding an object
//! and walking [`Record::rows`], and a schema has forgotten which type it came
//! from. So a connection is opened over the types themselves.
//!
//! What a type contributes is a value rather than a trait a caller implements.
//! [`Records::of`] takes `T` and keeps what the rebuild needs behind a private
//! trait, so `records![User, Note]` expands to ordinary calls and no caller ever
//! writes an impl.

use doclite_core::{FromStorage, Index, IndexId, IndexSchema};

use crate::error::DocumentError;
use crate::identity::{DocumentId, RevisionId};
use crate::record::{Codes, Record};
use crate::revision::Revision;
use crate::write::{self, Write};

/// Every record type a connection is opened over.
///
/// It answers the schemas `Connection` declares, and holds what a rebuild needs
/// to fill any index among them. Two types declaring the same index is
/// ordinary, and `Db::open` keeps the first of them.
///
/// ```
/// # use doclite::{records, Document};
/// # use serde::{Deserialize, Serialize};
/// #[derive(Clone, Document, Serialize, Deserialize)]
/// struct Note {
///     #[unique]
///     slug: String,
/// }
///
/// #[derive(Clone, Document, Serialize, Deserialize)]
/// struct Mark {
///     #[index]
///     note: String,
/// }
///
/// let records = records![Note, Mark];
/// assert_eq!(records.schemas().len(), 4, "two indexes each, generated one included");
/// ```
pub struct Records {
    types: Vec<Box<dyn Rebuilds>>,
}

impl Records {
    /// One record type.
    pub fn of<T: Record + 'static>() -> Self {
        Self {
            types: vec![Box::new(Held::<T>::new())],
        }
    }

    /// And another.
    pub fn and<T: Record + 'static>(mut self) -> Self {
        self.types.push(Box::new(Held::<T>::new()));
        self
    }

    /// Every index every type is filed under, in the order the types were
    /// named.
    ///
    /// This is what replaces a caller concatenating `A::schemas()` and
    /// `B::schemas()` by hand, which was the only way to open over two types.
    pub fn schemas(&self) -> Vec<IndexSchema> {
        self.types.iter().flat_map(|held| held.schemas()).collect()
    }

    /// How many record types this holds.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Whether it holds none, which `Connection` refuses to open over.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub(crate) fn types(&self) -> &[Box<dyn Rebuilds>] {
        &self.types
    }
}

/// One record type, as much of it as a rebuild needs.
///
/// Deliberately not public and deliberately not generic in its methods, so it
/// can be held as a trait object: a rebuild is driven by one loop over every
/// type, and the loop cannot be generic over each of them.
///
/// None of it needs an engine. An `Operation` is a value (ADR 0003), so
/// building the one that files a revision's missing rows is pure, and the
/// driver is the only part that has to run anything.
pub(crate) trait Rebuilds: Send + Sync {
    fn schemas(&self) -> Vec<IndexSchema>;

    fn latest(&self) -> Index<2>;

    /// The operation filing this revision's rows in the named indexes, and the
    /// document it belongs to.
    fn fill(&self, revision: Revised<'_>, codes: Codes) -> Result<Filled, DocumentError>;
}

/// One live revision as the store holds it, before anything has decoded it.
///
/// The bytes rather than a value, because the driver pages every type through
/// one loop and cannot name `T`.
pub(crate) struct Revised<'a> {
    pub(crate) object: &'a [u8],
    pub(crate) revision: RevisionId,
    pub(crate) wanted: &'a [IndexId],
}

/// What one revision contributes to a rebuild.
///
/// The document comes back alongside the operation because it is what paging
/// resumes from: `<record>_latest` is keyed on it, so the next page starts
/// strictly after the last document this one saw, and nothing else in the
/// driver has decoded enough to know it.
pub(crate) struct Filled {
    pub(crate) write: Write,
    pub(crate) document: DocumentId,
}

struct Held<T>(std::marker::PhantomData<fn() -> T>);

impl<T> Held<T> {
    fn new() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T: Record + 'static> Rebuilds for Held<T> {
    fn schemas(&self) -> Vec<IndexSchema> {
        T::schemas()
    }

    fn latest(&self) -> Index<2> {
        T::latest()
    }

    fn fill(&self, revised: Revised<'_>, codes: Codes) -> Result<Filled, DocumentError> {
        let envelope: Revision<T> = Revision::from_storage(revised.object)?;
        let document = envelope.id();
        let value: &T = &envelope;
        Ok(Filled {
            write: write::backfill(value, document, revised.revision, revised.wanted, codes),
            document,
        })
    }
}

/// Every record type a connection is opened over.
///
/// `records![User, Note]` is [`Records::of`] followed by one [`Records::and`]
/// per type after the first. It is a macro only so the types read as a list.
#[macro_export]
macro_rules! records {
    ($first:ty $(, $rest:ty)* $(,)?) => {
        $crate::Records::of::<$first>()$(.and::<$rest>())*
    };
}
