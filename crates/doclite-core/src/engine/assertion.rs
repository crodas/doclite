//! What an operation saw, and what it means later.
//!
//! There is no transaction handle, so what carries consistency from the reads
//! to the write is an assertion list rather than a held lock. That rule has two
//! halves and they have to agree: the executor decides what to record while it
//! walks, and whoever applies the batch decides what re-evaluating it means.
//! Both live here, because when they lived apart they disagreed and nothing
//! could see it.

use crate::model::index::IndexId;
use crate::model::object::ObjectId;
use crate::model::row::Row;
use crate::operation::step::Expect;

use super::key_range::KeyRange;

/// What an operation saw while it was building its batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// The range held nothing.
    Absent,
    /// The range held this many rows, which is cheaper to re-check than the rows
    /// themselves.
    Count(u64),
    /// These exact rows, in index order. What a count degrades to once this
    /// operation has staged into the same index.
    Rows(Vec<Row>),
}

impl Observed {
    /// What a precondition should assert, having read `committed` from the
    /// store.
    ///
    /// `staged` is whether this operation has already put something into the
    /// index. Once it has, a count no longer says what was observed, so the
    /// whole row set is asserted instead.
    pub fn recorded(expect: &Expect, staged: bool, committed: &[Row]) -> Self {
        if staged {
            return Self::Rows(committed.to_vec());
        }
        match expect {
            Expect::Absent => Self::Absent,
            Expect::Count(_) => Self::Count(committed.len() as u64),
        }
    }

    /// Whether this still describes the store.
    ///
    /// `rows` are the assertion's range as the store now holds it, in index
    /// order and before the batch. `limit` is what the reader looked under: it
    /// saw that many rows and no further, so a change past them is not a change
    /// it could have depended on.
    pub fn holds(&self, limit: Option<usize>, rows: &[Row]) -> bool {
        let rows = match limit {
            Some(limit) => &rows[..limit.min(rows.len())],
            None => rows,
        };
        match self {
            Self::Absent => rows.is_empty(),
            Self::Count(count) => rows.len() as u64 == *count,
            Self::Rows(seen) => rows == seen.as_slice(),
        }
    }
}

/// A condition a batch is refused for no longer holding.
///
/// Evaluated against the state **before** the batch. That is what lets one
/// condition serve both "nothing may hold this key" and "this key, which this
/// writer is itself about to release", without the two wanting different
/// moments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assertion {
    /// A range of one index, as the reader looked at it.
    Index {
        /// Which index the range reads.
        index: IndexId,
        /// The range, already resolved to bytes.
        range: KeyRange,
        /// How many rows the reader looked at, if it stopped early.
        limit: Option<usize>,
        /// What the reader saw there.
        observed: Observed,
    },
    /// One object, which is either there or not.
    Object {
        /// Which object.
        id: ObjectId,
        /// Whether the reader found it.
        present: bool,
    },
}

impl Assertion {
    /// The whole of an index assertion, for a store that has fetched the range.
    pub fn index(
        index: IndexId,
        range: KeyRange,
        limit: Option<usize>,
        observed: Observed,
    ) -> Self {
        Self::Index {
            index,
            range,
            limit,
            observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(names: &[(&str, u64)]) -> Vec<Row> {
        names
            .iter()
            .map(|(key, id)| Row::new(vec![key.as_bytes().to_vec()], ObjectId(*id)))
            .collect()
    }

    #[test]
    fn absent_holds_only_on_an_empty_range() {
        assert!(Observed::Absent.holds(None, &[]));
        assert!(!Observed::Absent.holds(None, &rows(&[("a", 1)])));
    }

    #[test]
    fn a_count_holds_while_the_count_does() {
        let observed = Observed::Count(2);
        assert!(observed.holds(None, &rows(&[("a", 1), ("b", 2)])));
        assert!(!observed.holds(None, &rows(&[("a", 1)])));
        assert!(!observed.holds(None, &rows(&[("a", 1), ("b", 2), ("c", 3)])));
    }

    #[test]
    fn a_row_set_holds_only_against_the_same_rows() {
        let seen = rows(&[("a", 1), ("b", 2)]);
        let observed = Observed::Rows(seen.clone());
        assert!(observed.holds(None, &seen));
        assert!(!observed.holds(None, &rows(&[("a", 1), ("b", 3)])));
    }

    /// The `for_each` case: a reader that stopped at `limit` never saw the rows
    /// past it, so a change there is not a conflict.
    #[test]
    fn a_limited_reader_is_judged_on_what_it_looked_at() {
        let observed = Observed::Rows(rows(&[("a", 1), ("b", 2)]));
        assert!(observed.holds(Some(2), &rows(&[("a", 1), ("b", 2), ("c", 3)])));
        assert!(!observed.holds(None, &rows(&[("a", 1), ("b", 2), ("c", 3)])));
    }

    #[test]
    fn a_limit_past_the_end_of_the_range_bounds_nothing() {
        let seen = rows(&[("a", 1)]);
        assert!(Observed::Rows(seen.clone()).holds(Some(9), &seen));
        assert!(Observed::Absent.holds(Some(9), &[]));
    }

    #[test]
    fn a_change_inside_the_limit_is_still_a_conflict() {
        let observed = Observed::Rows(rows(&[("a", 1), ("b", 2)]));
        assert!(!observed.holds(Some(2), &rows(&[("a", 1), ("c", 3), ("b", 2)])));
    }

    #[test]
    fn a_staged_index_is_asserted_by_its_whole_row_set() {
        let committed = rows(&[("a", 1)]);
        assert_eq!(
            Observed::recorded(&Expect::Absent, true, &committed),
            Observed::Rows(committed.clone())
        );
        assert_eq!(
            Observed::recorded(&Expect::Count(1), true, &committed),
            Observed::Rows(committed)
        );
    }

    #[test]
    fn an_untouched_index_is_asserted_by_what_was_expected() {
        let committed = rows(&[("a", 1)]);
        assert_eq!(
            Observed::recorded(&Expect::Absent, false, &committed),
            Observed::Absent
        );
        assert_eq!(
            Observed::recorded(&Expect::Count(7), false, &committed),
            Observed::Count(1)
        );
    }
}
