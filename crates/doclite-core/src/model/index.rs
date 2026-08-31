//! What an index is: a name over a fixed number of typed columns, and no rule.

use std::fmt;
use std::marker::PhantomData;

/// Which index.
///
/// A `&'static str` because an index is named at compile time. That is also why
/// the `serde` feature derives `Serialize` and not `Deserialize`: no
/// deserializer can hand back a `'static` borrow. An operation can be encoded
/// and sent; a receiver rebuilds it against its own catalog of names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct IndexId(pub &'static str);

impl fmt::Display for IndexId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// What a column holds, for an engine that can lay it out as more than bytes.
///
/// Ordering is not settled here and never is: [`ToOrdBytes`] already produces
/// bytes that sort the way the value does, and an engine compares those (ADR
/// 0002). A column type says what a column *is*, so a store with a type system
/// of its own can declare a real column and query it in its own language. An
/// engine that keeps ordered bytes ignores this entirely.
///
/// [`ToOrdBytes`]: crate::ToOrdBytes
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ColumnType {
    /// One byte: `false` sorts before `true`.
    Bool,
    /// A Unicode scalar value, which encodes as the `u32` behind it.
    Char,
    /// Eight bits, unsigned.
    U8,
    /// Sixteen bits, unsigned.
    U16,
    /// Thirty-two bits, unsigned.
    U32,
    /// Sixty-four bits, unsigned.
    U64,
    /// A hundred and twenty-eight bits, unsigned.
    U128,
    /// Eight bits, signed.
    I8,
    /// Sixteen bits, signed.
    I16,
    /// Thirty-two bits, signed.
    I32,
    /// Sixty-four bits, signed, and what an `ObjectId` declares itself as.
    I64,
    /// A hundred and twenty-eight bits, signed.
    I128,
    /// UTF-8 text, compared as the bytes it already is.
    Str,
    /// The fallback, and what a type nobody has described arrives as: bytes an
    /// engine may compare and must not interpret.
    Bytes,
    /// A column that may be absent, which is what `Option` encodes.
    Nullable(&'static ColumnType),
}

/// An index handle whose arity is part of its type.
///
/// Carrying the arity as a const parameter is what turns "this key has the
/// wrong number of components" from a runtime refusal into a compile error.
///
/// An index is a set of `(columns, object)` rows and carries no rule.
/// Whether at most one object may hold a key is asserted by an operation,
/// never declared here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Index<const N: usize> {
    id: IndexId,
    columns: &'static [ColumnType],
    arity: PhantomData<[(); N]>,
}

impl<const N: usize> Index<N> {
    /// An index of `N` columns an engine may only compare.
    ///
    /// The byte-level API's constructor: a caller working in bytes has no types
    /// to declare, so every column is [`ColumnType::Bytes`]. Use
    /// [`Index::typed`] where the types are known; `#[derive(Object)]` does.
    pub const fn new(name: &'static str) -> Self {
        Self {
            id: IndexId(name),
            columns: const { &[ColumnType::Bytes; N] },
            arity: PhantomData,
        }
    }

    /// An index whose columns say what they hold.
    pub const fn typed(name: &'static str, columns: &'static [ColumnType; N]) -> Self {
        Self {
            id: IndexId(name),
            columns,
            arity: PhantomData,
        }
    }

    /// Which index, for a step that names one rather than holds one.
    pub const fn id(&self) -> IndexId {
        self.id
    }

    /// The arity as a value, for code that cannot be generic over `N`.
    pub const fn arity(&self) -> usize {
        N
    }

    /// What an engine is told: this handle with the arity dropped, because a
    /// schema is checked against rather than typed.
    pub const fn schema(&self) -> IndexSchema {
        IndexSchema {
            id: self.id,
            columns: self.columns,
        }
    }
}

/// What an engine is told about one index, which is all it needs to lay one
/// out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct IndexSchema {
    /// Which index this describes.
    pub id: IndexId,
    /// What each column holds, and by its length how many there are.
    pub columns: &'static [ColumnType],
}

impl IndexSchema {
    /// How many columns a key of this index has.
    pub const fn arity(&self) -> usize {
        self.columns.len()
    }
}
