//! What an operation has staged but not yet committed.
//!
//! The overlay is what makes read-your-own-writes work without a transaction
//! handle. A precondition reads the engine, folds the staged mutations over the
//! answer, and judges the result; the engine itself sees nothing until `apply`.

use crate::engine::{Assertion, Batch, IndexEntry, KeyRange, RangeDelete};
use crate::model::index::IndexId;
use crate::model::object::ObjectId;
use crate::model::row::Row;

/// The staged work, in the shape a batch carries it.
pub(crate) struct Staged {
    objects: Vec<(ObjectId, Vec<u8>)>,
    delete: Vec<RangeDelete>,
    insert: Vec<IndexEntry>,
}

impl Staged {
    /// The staged work and the assertions it is conditional on, as the one
    /// thing an engine is handed.
    ///
    /// The batch borrows all of it, so it is built here rather than field by
    /// field somewhere that would have to name every part again.
    pub(crate) fn batch<'a>(&'a self, asserts: &'a [Assertion]) -> Batch<'a> {
        Batch::Writes {
            asserts,
            objects: &self.objects,
            delete: &self.delete,
            insert: &self.insert,
        }
    }
}

enum Mutation {
    Insert(IndexEntry),
    Delete(RangeDelete),
}

/// Nothing here knows about a sweep. One is the whole operation, so it never
/// sits beside staged work and never goes through the overlay.
#[derive(Default)]
pub(crate) struct Delta {
    objects: Vec<(ObjectId, Vec<u8>)>,
    log: Vec<Mutation>,
}

impl Delta {
    /// Whether nothing is staged at all, in which case there is nothing to
    /// hand an engine and `apply` is skipped.
    pub(crate) fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.log.is_empty()
    }

    pub(crate) fn put_object(&mut self, id: ObjectId, bytes: Vec<u8>) {
        self.objects.push((id, bytes));
    }

    pub(crate) fn insert(&mut self, entry: IndexEntry) {
        self.log.push(Mutation::Insert(entry));
    }

    pub(crate) fn delete(&mut self, delete: RangeDelete) {
        self.log.push(Mutation::Delete(delete));
    }

    pub(crate) fn staged_object(&self, id: ObjectId) -> Option<&[u8]> {
        self.objects
            .iter()
            .find(|(staged, _)| *staged == id)
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// Whether anything staged could change what this index answers.
    pub(crate) fn touches(&self, index: IndexId) -> bool {
        self.log.iter().any(|mutation| match mutation {
            Mutation::Insert(entry) => entry.index == index,
            Mutation::Delete(delete) => delete.index == index,
        })
    }

    /// The rows of a range as this operation now sees them.
    ///
    /// Replayed in written order rather than collapsed, because a delete and an
    /// insert over the same key mean opposite things depending on which came
    /// first.
    pub(crate) fn rows_in(
        &self,
        index: IndexId,
        range: &KeyRange,
        committed: Vec<Row>,
    ) -> Vec<Row> {
        let mut rows: Vec<Row> = committed
            .into_iter()
            .filter(|row| range.contains(row.key.components()))
            .collect();

        for mutation in &self.log {
            match mutation {
                Mutation::Delete(delete) if delete.index == index => {
                    rows.retain(|row| !delete.range.contains(row.key.components()));
                }
                Mutation::Insert(entry) if entry.index == index => {
                    if range.contains(&entry.columns) {
                        let row = Row::new(entry.columns.clone(), entry.object);
                        if !rows.contains(&row) {
                            rows.push(row);
                        }
                    }
                }
                Mutation::Delete(_) | Mutation::Insert(_) => {}
            }
        }

        rows.sort();
        rows
    }

    /// The staged work as the three things a batch carries.
    ///
    /// An insert a later delete covers is dropped here rather than reordered,
    /// which is what lets the engine apply every delete before every insert and
    /// still get the written order's meaning.
    pub(crate) fn into_batch(self) -> Staged {
        let mut deletes = Vec::new();
        let mut inserts = Vec::new();

        for (at, mutation) in self.log.iter().enumerate() {
            match mutation {
                Mutation::Delete(delete) => deletes.push(delete.clone()),
                Mutation::Insert(entry) => {
                    let retired = self.log[at + 1..].iter().any(|later| match later {
                        Mutation::Delete(delete) => {
                            delete.index == entry.index && delete.range.contains(&entry.columns)
                        }
                        Mutation::Insert(_) => false,
                    });
                    if !retired {
                        inserts.push(entry.clone());
                    }
                }
            }
        }

        Staged {
            objects: self.objects,
            delete: deletes,
            insert: inserts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ONE: IndexId = IndexId("one");
    static OTHER: IndexId = IndexId("other");

    fn entry(key: &str, id: u64) -> IndexEntry {
        IndexEntry {
            index: ONE,
            columns: vec![key.as_bytes().to_vec()],
            object: ObjectId(id),
        }
    }

    fn covering(index: IndexId, key: &str) -> RangeDelete {
        RangeDelete {
            index,
            range: KeyRange::Exact(vec![key.as_bytes().to_vec()]),
        }
    }

    /// ADR 0005's load-bearing claim: an engine may apply every delete before
    /// every insert, which is only the written order's meaning because the
    /// insert a later delete covers is dropped here first.
    #[test]
    fn an_insert_a_later_delete_covers_is_dropped() {
        let mut delta = Delta::default();
        delta.insert(entry("a", 1));
        delta.delete(covering(ONE, "a"));

        let staged = delta.into_batch();
        assert!(
            staged.insert.is_empty(),
            "the insert would have been retired"
        );
        assert_eq!(staged.delete.len(), 1);
    }

    #[test]
    fn an_insert_after_the_delete_that_would_have_covered_it_survives() {
        let mut delta = Delta::default();
        delta.delete(covering(ONE, "a"));
        delta.insert(entry("a", 1));

        let staged = delta.into_batch();
        assert_eq!(staged.insert.len(), 1, "the delete came first");
    }

    /// The range has to name the same index, or a cascade over one index would
    /// silently drop rows filed into another.
    #[test]
    fn a_delete_of_another_index_covers_nothing() {
        let mut delta = Delta::default();
        delta.insert(entry("a", 1));
        delta.delete(covering(OTHER, "a"));

        assert_eq!(delta.into_batch().insert.len(), 1);
    }

    #[test]
    fn a_delta_that_staged_nothing_is_empty() {
        let mut delta = Delta::default();
        assert!(delta.is_empty());
        delta.insert(entry("a", 1));
        assert!(!delta.is_empty());
    }
}
