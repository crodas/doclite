//! Which rows of an index an operation is talking about.

use crate::model::column::Column;
use crate::model::key::Key;

/// Whether an endpoint is part of the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Bound {
    /// The endpoint's own key is in the range.
    Incl,
    /// The endpoint's own key is not.
    Excl,
}

/// A range as a step holds it, whose components may still be row slots.
///
/// [`KeyRange`](crate::KeyRange) is the same idea once every component is
/// bytes. The two are kept apart so a slot can never reach an engine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum RangeSpec {
    /// Every row of the index.
    All,
    /// One key, every component given.
    Exact(Vec<Column>),
    /// Every key starting with these components, which is fewer than the index
    /// has.
    Prefix(Vec<Column>),
    /// An endpoint that is `None` is not a bound at all, which is how `>= 18`
    /// and `< 65` are written without inventing a maximum for the column type.
    Between {
        /// The low end, or `None` for unbounded below.
        lo: Option<(Vec<Column>, Bound)>,
        /// The high end, or `None` for unbounded above.
        hi: Option<(Vec<Column>, Bound)>,
    },
}

impl RangeSpec {
    /// Every component the range names, whichever shape it is, so a caller
    /// checking for row slots need not match on the variant.
    pub fn columns(&self) -> impl Iterator<Item = &Column> {
        let (a, b) = match self {
            Self::All => (None, None),
            Self::Exact(c) | Self::Prefix(c) => (Some(c), None),
            Self::Between { lo, hi } => (
                lo.as_ref().map(|(columns, _)| columns),
                hi.as_ref().map(|(columns, _)| columns),
            ),
        };
        a.into_iter().chain(b).flatten()
    }
}

/// Rows of an arity-`N` index, named by a `K`-component key.
///
/// Carrying both counts in the type is what makes [`exact`] on a partial key,
/// and [`prefix`] with more components than the index has, compile errors
/// rather than refusals at run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Range<const N: usize, const K: usize> {
    spec: RangeSpec,
}

impl<const N: usize, const K: usize> Range<N, K> {
    /// Wraps a spec a caller assembled itself.
    ///
    /// The two counts are a guard on the constructors below, and a range whose
    /// ends name different numbers of components cannot state one of them. That
    /// range is what a compound index needs when only its last component is
    /// ranged, so building the spec directly is the way to say it.
    pub fn from_spec(spec: RangeSpec) -> Self {
        Self { spec }
    }

    /// The spec, given up for a step that has no use for the two counts.
    pub fn into_spec(self) -> RangeSpec {
        self.spec
    }

    /// The spec, for reading a range without consuming it.
    pub fn spec(&self) -> &RangeSpec {
        &self.spec
    }
}

/// One key and nothing else.
pub fn exact<const N: usize>(key: Key<N>) -> Range<N, N> {
    Range {
        spec: RangeSpec::Exact(key.into_columns()),
    }
}

/// Every key that starts with these components.
pub fn prefix<const N: usize, const K: usize>(key: Key<K>) -> Range<N, K> {
    const { assert!(K <= N, "a prefix cannot be longer than the index it reads") };
    Range {
        spec: RangeSpec::Prefix(key.into_columns()),
    }
}

/// Every key between two bounds, either of which may be absent.
///
/// An absent endpoint is not a bound, so `bounded(Some(..), None)` is `>=` and
/// `bounded(None, Some(..))` is `<`. The alternative is asking every column
/// type for a maximum, which `String` does not have.
pub fn bounded<const N: usize, const K: usize>(
    lo: Option<(Key<K>, Bound)>,
    hi: Option<(Key<K>, Bound)>,
) -> Range<N, K> {
    const { assert!(K <= N, "a bound cannot be longer than the index it reads") };
    Range {
        spec: RangeSpec::Between {
            lo: lo.map(|(key, bound)| (key.into_columns(), bound)),
            hi: hi.map(|(key, bound)| (key.into_columns(), bound)),
        },
    }
}

/// Every key between two bounds.
pub fn between<const N: usize, const K: usize>(
    lo: Key<K>,
    hi: Key<K>,
    lo_bound: Bound,
    hi_bound: Bound,
) -> Range<N, K> {
    bounded(Some((lo, lo_bound)), Some((hi, hi_bound)))
}

/// Every row of the index.
pub fn all<const N: usize>() -> Range<N, 0> {
    Range {
        spec: RangeSpec::All,
    }
}
