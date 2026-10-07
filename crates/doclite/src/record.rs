//! What a type owes before the store can file and find it.
//!
//! A record does not build operations. It walks its index rows once and hands
//! each one to whoever asked, which is what lets the derive keep using
//! `doclite-core`'s const-generic API, where an index of the wrong width is a
//! compile error rather than something checked again at run time. What to do
//! with a row, and in what order, is settled in one place above it and nowhere
//! else.

use doclite_core::{AbortCode, FromStorage, Index, IndexId, IndexSchema, Key, ToStorage};

use crate::identity::{DocumentId, RevisionId};

/// The first abort code this layer raises, well clear of what `doclite-core`
/// reserves and of the range a caller is likely to have taken.
pub(crate) const CODE_BASE: u32 = 1000;

/// Why one staged write refused, told apart from every other write's reasons.
///
/// A write owns a run of codes starting at its base: the first stands for a
/// revision that has already been superseded, and the rest name the unique
/// indexes it claimed, in the order the walk claimed them. The base is what
/// keeps two writes composed into one operation from both answering to 1000,
/// which is the whole reason a code is not a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Codes {
    base: u32,
}

/// What a code in a write's run stood for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    Stale,
    Duplicate(usize),
}

impl Codes {
    pub(crate) const fn at(base: u32) -> Self {
        Self { base }
    }

    pub(crate) fn stale(&self) -> AbortCode {
        AbortCode::new(self.base)
    }

    pub(crate) fn duplicate(&self, at: usize) -> AbortCode {
        AbortCode::new(self.base + 1 + at as u32)
    }

    /// How many codes a write claiming this many unique indexes takes up, so
    /// the next write in an operation can start past them.
    pub(crate) fn span(claimed: usize) -> u32 {
        1 + claimed as u32
    }

    pub(crate) fn reason(&self, code: AbortCode, claimed: usize) -> Option<Reason> {
        let offset = code.0.checked_sub(self.base)?;
        match offset {
            0 => Some(Reason::Stale),
            _ if (offset as usize) <= claimed => Some(Reason::Duplicate(offset as usize - 1)),
            _ => None,
        }
    }
}

/// A type the store can file and find.
///
/// `#[derive(Document)]` writes every method of this.
pub trait Record: ToStorage + FromStorage + Clone + Send + Sync + Sized {
    /// Every index this type is filed under, for `Connection::open`.
    fn schemas() -> Vec<IndexSchema>;

    /// The generated index holding one row per live record.
    ///
    /// It is the one index this layer names itself, because asking whether a
    /// revision is still current is a question about ids rather than about
    /// anything the record holds.
    fn latest() -> Index<2>;

    /// Walks every index row this value belongs in, once each.
    ///
    /// The walk is the whole of what a record knows about its own storage.
    /// Filing rows and retiring them are the same walk with different [`Rows`],
    /// which is why they cannot disagree about a key.
    fn rows<R: Rows>(&self, document: DocumentId, revision: RevisionId, into: &mut R);
}

/// A record whose revisions may be superseded.
///
/// `#[derive(Document)]` writes this unless the struct says `#[immutable]`, so
/// a record that has exactly one revision refuses an edit where the edit was
/// written rather than when it runs. Whether a record can be superseded is a
/// fact about the schema, so nothing about it reaches an engine and there is no
/// abort code for it.
///
/// It is deliberately not a subtrait of [`Record`]. What it gates is
/// [`Stored`]'s `DerefMut`, and a `Stored` holds any value the store can hold.
///
/// [`Stored`]: crate::Stored
///
/// ```
/// use doclite::{Connection, Document, DocumentError, Record, records};
/// use doclite_memory::MemoryEngine;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Debug, Clone, Serialize, Deserialize, Document)]
/// struct Note {
///     #[index]
///     shelf: u8,
/// }
///
/// # async fn example() -> Result<(), DocumentError> {
/// let conn = Connection::open(MemoryEngine::new(), records![Note]).await?;
/// let mut note = conn.insert(Note { shelf: 1 }).await?;
/// note.shelf = 2;
/// conn.update(note).await?;
/// # Ok(())
/// # }
/// # tokio::runtime::Builder::new_current_thread()
/// #     .build()
/// #     .expect("a current-thread runtime")
/// #     .block_on(example())
/// #     .expect("the example runs");
/// ```
///
/// The same struct marked `#[immutable]` does not compile:
///
/// ```compile_fail
/// use doclite::{Connection, Document, DocumentError, Record, records};
/// use doclite_memory::MemoryEngine;
/// use serde::{Deserialize, Serialize};
///
/// #[derive(Debug, Clone, Serialize, Deserialize, Document)]
/// #[immutable]
/// struct Entry {
///     #[index]
///     shelf: u8,
/// }
///
/// # async fn example() -> Result<(), DocumentError> {
/// let conn = Connection::open(MemoryEngine::new(), records![Entry]).await?;
/// let mut entry = conn.insert(Entry { shelf: 1 }).await?;
/// entry.shelf = 2;
/// conn.update(entry).await?;
/// # Ok(())
/// # }
/// ```
pub trait Amendable {}

/// What a walk of a record's index rows is handed, once per index.
///
/// `row` is generic over the width rather than taking a slice of columns,
/// because a `Key<N>` is an array and an `Operation` will not file an index row
/// whose arity it cannot see at compile time.
pub trait Rows {
    /// Called once per row the record belongs in. `unique` and `name` are what
    /// a duplicate is refused and named by, since an index itself carries no
    /// rule.
    fn row<const N: usize>(
        &mut self,
        index: &Index<N>,
        key: Key<N>,
        name: &'static str,
        unique: bool,
    );
}

/// A walk narrowed to some of a record's indexes, forwarding to another
/// [`Rows`].
///
/// A rebuild files the rows of the indexes that are missing them and must leave
/// every other index alone, because those rows are already there and filing one
/// twice is a second row under one key. Narrowing the walk rather than writing a
/// second one is what keeps `Retire`, `File` and `Claim` the only places that
/// know what a row becomes: they cannot disagree with a rebuild about a key,
/// because the rebuild is them.
pub struct Only<'a, R> {
    into: &'a mut R,
    wanted: &'a [IndexId],
}

impl<'a, R: Rows> Only<'a, R> {
    /// Forwards only the rows belonging to these indexes.
    pub fn these(into: &'a mut R, wanted: &'a [IndexId]) -> Self {
        Self { into, wanted }
    }
}

impl<R: Rows> Rows for Only<'_, R> {
    fn row<const N: usize>(
        &mut self,
        index: &Index<N>,
        key: Key<N>,
        name: &'static str,
        unique: bool,
    ) {
        if self.wanted.contains(&index.id()) {
            self.into.row(index, key, name, unique);
        }
    }
}
