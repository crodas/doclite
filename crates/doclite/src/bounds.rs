//! What a `by_` method accepts: a value, or a comparison against one.
//!
//! A scalar is an equality and a Rust range is a comparison, so one generated
//! method covers both. The column type is a parameter on [`IntoBounds`] rather
//! than only on `Self`, which is what keeps the scalar impl from overlapping
//! the range impls: unifying them would need `C = Range<C>`.

use std::marker::PhantomData;
use std::ops::{Range, RangeFrom, RangeInclusive, RangeTo, RangeToInclusive};

use doclite_core::{Bound, Column, IntoColumn};

/// Which values of one column a query is asking about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bounds {
    /// One value, which is what a bare literal means.
    Exact(Column),
    /// A span, either end of which may be open.
    Range {
        /// The low end, or `None` for unbounded below.
        lo: Option<(Column, Bound)>,
        /// The high end, or `None` for unbounded above.
        hi: Option<(Column, Bound)>,
    },
}

/// `> value`, the one comparison Rust range syntax cannot spell: there is no
/// exclusive-start literal. `<`, `<=` and `>=` are `..v`, `..=v` and `v..`.
///
/// It carries the column type so the value is checked against the column the
/// method reads. An untyped constructor would take a literal at whatever type
/// inference defaulted to, and a `25` that encoded as `i32` against a `u8`
/// column compares as four bytes against one and quietly matches nothing.
pub fn greater_than<C: IntoColumn>(value: C) -> Comparison<C> {
    Comparison {
        bounds: Bounds::Range {
            lo: Some((value.into_column(), Bound::Excl)),
            hi: None,
        },
        column: PhantomData,
    }
}

/// A comparison that remembers which column type it was built for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison<C> {
    bounds: Bounds,
    column: PhantomData<fn() -> C>,
}

impl<C: IntoColumn> IntoBounds<C> for Comparison<C> {
    fn into_bounds(self) -> Bounds {
        self.bounds
    }
}

/// A value or a range over a column of type `C`.
///
/// | Written | Means |
/// |---|---|
/// | `18` | `= 18` |
/// | `18..` | `>= 18` |
/// | `..65` | `< 65` |
/// | `..=65` | `<= 65` |
/// | `18..65` | `>= 18` and `< 65` |
/// | `18..=65` | `>= 18` and `<= 65` |
///
/// `..` is deliberately absent: not calling the method at all already means
/// every row, and an impl for `RangeFull` would collide with the scalar one.
pub trait IntoBounds<C> {
    /// The column type is on the trait rather than the method, so a literal is
    /// encoded at the column's type instead of whatever inference picked.
    fn into_bounds(self) -> Bounds;
}

impl<C: IntoColumn> IntoBounds<C> for C {
    fn into_bounds(self) -> Bounds {
        Bounds::Exact(self.into_column())
    }
}

impl<C: IntoColumn> IntoBounds<C> for Range<C> {
    fn into_bounds(self) -> Bounds {
        Bounds::Range {
            lo: Some((self.start.into_column(), Bound::Incl)),
            hi: Some((self.end.into_column(), Bound::Excl)),
        }
    }
}

impl<C: IntoColumn> IntoBounds<C> for RangeInclusive<C> {
    fn into_bounds(self) -> Bounds {
        let (start, end) = self.into_inner();
        Bounds::Range {
            lo: Some((start.into_column(), Bound::Incl)),
            hi: Some((end.into_column(), Bound::Incl)),
        }
    }
}

impl<C: IntoColumn> IntoBounds<C> for RangeFrom<C> {
    fn into_bounds(self) -> Bounds {
        Bounds::Range {
            lo: Some((self.start.into_column(), Bound::Incl)),
            hi: None,
        }
    }
}

impl<C: IntoColumn> IntoBounds<C> for RangeTo<C> {
    fn into_bounds(self) -> Bounds {
        Bounds::Range {
            lo: None,
            hi: Some((self.end.into_column(), Bound::Excl)),
        }
    }
}

impl<C: IntoColumn> IntoBounds<C> for RangeToInclusive<C> {
    fn into_bounds(self) -> Bounds {
        Bounds::Range {
            lo: None,
            hi: Some((self.end.into_column(), Bound::Incl)),
        }
    }
}
