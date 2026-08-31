//! A range with every component resolved to bytes.

use std::cmp::Ordering;

use crate::model::range::Bound;
use crate::model::row::Row;

/// A range with every component resolved to bytes, which is what an engine
/// sees.
///
/// The caller's half is [`RangeSpec`](crate::RangeSpec), whose components may
/// still be row slots. Keeping the two apart is what stops a slot from reaching
/// an engine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum KeyRange {
    /// Every row of the index.
    All,
    /// One key, every component given.
    Exact(Vec<Vec<u8>>),
    /// Every key starting with these components.
    Prefix(Vec<Vec<u8>>),
    /// An endpoint that is `None` is not a bound, so a range open at one end
    /// is `>=` or `<` rather than a comparison against an invented extreme.
    Between {
        /// The low end, or `None` for unbounded below.
        lo: Option<(Vec<Vec<u8>>, Bound)>,
        /// The high end, or `None` for unbounded above.
        hi: Option<(Vec<Vec<u8>>, Bound)>,
    },
}

impl KeyRange {
    /// Whether a row's columns fall in the range.
    ///
    /// A partial bound is compared over the components it names and no further,
    /// so a two-component bound on a three-column index selects whole groups.
    pub fn contains(&self, columns: &[Vec<u8>]) -> bool {
        match self {
            Self::All => true,
            Self::Exact(key) => columns == key.as_slice(),
            Self::Prefix(key) => {
                columns.len() >= key.len() && &columns[..key.len()] == key.as_slice()
            }
            Self::Between { lo, hi } => {
                let above = match lo {
                    None => true,
                    Some((key, bound)) => match compare(columns, key) {
                        Ordering::Less => false,
                        Ordering::Equal => *bound == Bound::Incl,
                        Ordering::Greater => true,
                    },
                };
                let below = match hi {
                    None => true,
                    Some((key, bound)) => match compare(columns, key) {
                        Ordering::Greater => false,
                        Ordering::Equal => *bound == Bound::Incl,
                        Ordering::Less => true,
                    },
                };
                above && below
            }
        }
    }

    /// The rows of a set that fall in the range.
    ///
    /// An engine may answer with more than the range selects, so the executor
    /// filters. This is the line that makes every scan argument a hint.
    pub fn select(&self, rows: Vec<Row>) -> Vec<Row> {
        rows.into_iter()
            .filter(|row| self.contains(row.key.components()))
            .collect()
    }
}

fn compare(columns: &[Vec<u8>], bound: &[Vec<u8>]) -> Ordering {
    let shared = bound.len().min(columns.len());
    columns[..shared].cmp(&bound[..shared])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(values: &[&str]) -> Vec<Vec<u8>> {
        values.iter().map(|v| v.as_bytes().to_vec()).collect()
    }

    #[test]
    fn exact_wants_the_whole_key() {
        let range = KeyRange::Exact(cols(&["a", "b"]));
        assert!(range.contains(&cols(&["a", "b"])));
        assert!(!range.contains(&cols(&["a"])));
        assert!(!range.contains(&cols(&["a", "b", "c"])));
    }

    #[test]
    fn prefix_takes_the_group_under_it() {
        let range = KeyRange::Prefix(cols(&["a"]));
        assert!(range.contains(&cols(&["a", "b"])));
        assert!(range.contains(&cols(&["a"])));
        assert!(!range.contains(&cols(&["b"])));
    }

    #[test]
    fn between_honours_each_bound_on_its_own() {
        let range = KeyRange::Between {
            lo: Some((cols(&["b"]), Bound::Incl)),
            hi: Some((cols(&["d"]), Bound::Excl)),
        };
        assert!(range.contains(&cols(&["b"])));
        assert!(range.contains(&cols(&["c"])));
        assert!(!range.contains(&cols(&["d"])));
        assert!(!range.contains(&cols(&["a"])));
    }

    #[test]
    fn a_partial_bound_selects_whole_groups() {
        let range = KeyRange::Between {
            lo: Some((cols(&["b"]), Bound::Incl)),
            hi: Some((cols(&["b"]), Bound::Incl)),
        };
        assert!(range.contains(&cols(&["b", "anything"])));
        assert!(!range.contains(&cols(&["c", "anything"])));
    }

    #[test]
    fn an_absent_upper_endpoint_bounds_nothing() {
        let range = KeyRange::Between {
            lo: Some((cols(&["b"]), Bound::Incl)),
            hi: None,
        };
        assert!(!range.contains(&cols(&["a"])));
        assert!(range.contains(&cols(&["b"])));
        assert!(range.contains(&cols(&["zzz"])));
    }

    #[test]
    fn an_absent_lower_endpoint_bounds_nothing() {
        let range = KeyRange::Between {
            lo: None,
            hi: Some((cols(&["d"]), Bound::Excl)),
        };
        assert!(range.contains(&cols(&[""])));
        assert!(range.contains(&cols(&["c"])));
        assert!(!range.contains(&cols(&["d"])));
    }

    #[test]
    fn two_absent_endpoints_take_everything() {
        let range = KeyRange::Between { lo: None, hi: None };
        assert!(range.contains(&cols(&["anything"])));
        assert!(range.contains(&[]));
    }
}
