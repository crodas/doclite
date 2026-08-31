//! Whether a batch may land, and the order it lands in.
//!
//! This is what every engine used to write for itself. It is pure over a
//! [`Region`], so it is tested here against a hand-written one rather than
//! through a store, a runtime and an executor.

use doclite_core::{Assertion, Batch, IndexEntry, ObjectId, RangeDelete, Row, StorageError};

use crate::store::Region;

/// One batch being judged against one region, and then written to it.
///
/// The two halves are one type because they are one decision: everything that
/// could refuse the batch is asked while nothing has moved, and what follows
/// cannot fail. Holding both means the order cannot be got wrong by a caller
/// that has them as two loose functions.
pub struct Admission<'a, R> {
    region: &'a mut R,
    asserts: &'a [Assertion],
    objects: &'a [(ObjectId, Vec<u8>)],
    delete: &'a [RangeDelete],
    insert: &'a [IndexEntry],
}

impl<'a, R: Region> Admission<'a, R> {
    /// A store never takes a sweep, so being handed one means an engine said
    /// yes to work it cannot do.
    ///
    /// Refusing it here is what the match buys. A sweep used to be a field
    /// neither `admits` nor `commit` read, so a store that claimed the
    /// capability wrote nothing and reported success, which is the one failure
    /// in this crate that looked like success.
    pub fn over(region: &'a mut R, batch: &'a Batch<'a>) -> Result<Self, StorageError> {
        let Batch::Writes {
            asserts,
            objects,
            delete,
            insert,
        } = batch
        else {
            return Err(StorageError::Engine(
                "a store was handed a sweep, which only an engine that writes one is given".into(),
            ));
        };
        Ok(Self {
            region,
            asserts,
            objects,
            delete,
            insert,
        })
    }

    /// Everything about the batch that can fail, decided before anything moves.
    ///
    /// `Ok(false)` is a conflict: an assertion stopped holding and the executor
    /// will walk the operation again. `Err` is the store refusing the batch on
    /// its own terms, which no amount of re-walking fixes.
    pub fn admits(&self) -> Result<bool, StorageError> {
        for assertion in self.asserts {
            if !self.holds(assertion)? {
                return Ok(false);
            }
        }

        for (id, _) in self.objects {
            if self.region.has_object(*id)? {
                return Err(StorageError::DuplicateObject(*id));
            }
        }

        for delete in self.delete {
            self.region.arity(delete.index)?;
        }

        for entry in self.insert {
            let arity = self.region.arity(entry.index)?;
            if entry.columns.len() != arity {
                return Err(StorageError::Arity {
                    index: entry.index,
                    expected: arity,
                    found: entry.columns.len(),
                });
            }
        }

        Ok(true)
    }

    /// Deletes, then objects, then index rows, which is the order a batch
    /// promises. Nothing here can fail, which is what makes the batch atomic.
    pub fn commit(&mut self) {
        for delete in self.delete {
            self.region.delete(delete.index, &delete.range);
        }

        for (id, bytes) in self.objects {
            self.region.put_object(*id, bytes.clone());
        }

        for entry in self.insert {
            self.region.put_row(entry.index, row_of(entry));
        }
    }

    /// Whether one assertion still describes the region.
    ///
    /// The index is looked up first, so an assertion naming one nobody declared
    /// is an error rather than a conflict: re-walking cannot make it appear.
    fn holds(&self, assertion: &Assertion) -> Result<bool, StorageError> {
        match assertion {
            Assertion::Object { id, present } => Ok(self.region.has_object(*id)? == *present),
            Assertion::Index {
                index,
                range,
                limit,
                observed,
            } => {
                self.region.arity(*index)?;
                Ok(observed.holds(*limit, &self.region.rows(*index, range)?))
            }
        }
    }
}

fn row_of(entry: &IndexEntry) -> Row {
    Row::new(entry.columns.clone(), entry.object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use doclite_core::{IndexId, KeyRange, Observed, Sweep};
    use std::collections::{BTreeMap, BTreeSet};

    static ONE: IndexId = IndexId("one");
    static UNKNOWN: IndexId = IndexId("unknown");

    /// Judging a batch, so a test reads the way it did before the two halves
    /// became one type.
    fn judged(region: &mut Fake, batch: &Batch<'_>) -> Result<bool, StorageError> {
        Admission::over(region, batch)?.admits()
    }

    fn written(region: &mut Fake, batch: &Batch<'_>) {
        Admission::over(region, batch)
            .expect("these are writes")
            .commit();
    }

    #[derive(Default)]
    struct Fake {
        rows: BTreeSet<Row>,
        objects: BTreeMap<ObjectId, Vec<u8>>,
        deleted: Vec<KeyRange>,
        wrote: Vec<Row>,
        order: Vec<&'static str>,
    }

    impl Region for Fake {
        fn arity(&self, index: IndexId) -> Result<usize, StorageError> {
            if index == ONE {
                Ok(1)
            } else {
                Err(StorageError::UnknownIndex(index))
            }
        }

        fn rows(&self, _index: IndexId, range: &KeyRange) -> Result<Vec<Row>, StorageError> {
            Ok(self
                .rows
                .iter()
                .filter(|row| range.contains(row.key.components()))
                .cloned()
                .collect())
        }

        fn has_object(&self, id: ObjectId) -> Result<bool, StorageError> {
            Ok(self.objects.contains_key(&id))
        }

        fn delete(&mut self, _index: IndexId, range: &KeyRange) {
            self.order.push("delete");
            self.deleted.push(range.clone());
        }

        fn put_object(&mut self, id: ObjectId, bytes: Vec<u8>) {
            self.order.push("object");
            self.objects.insert(id, bytes);
        }

        fn put_row(&mut self, _index: IndexId, row: Row) {
            self.order.push("row");
            self.wrote.push(row);
        }
    }

    fn row(key: &str, id: u64) -> Row {
        Row::new(vec![key.as_bytes().to_vec()], ObjectId(id))
    }

    fn entry(key: &str, id: u64) -> IndexEntry {
        IndexEntry {
            index: ONE,
            columns: vec![key.as_bytes().to_vec()],
            object: ObjectId(id),
        }
    }

    fn batch<'a>(
        asserts: &'a [Assertion],
        objects: &'a [(ObjectId, Vec<u8>)],
        delete: &'a [RangeDelete],
        insert: &'a [IndexEntry],
    ) -> Batch<'a> {
        Batch::Writes {
            asserts,
            objects,
            delete,
            insert,
        }
    }

    #[test]
    fn an_assertion_that_still_holds_admits_the_batch() {
        let mut region = Fake::default();
        let asserts = [Assertion::index(
            ONE,
            KeyRange::Exact(vec![b"a".to_vec()]),
            None,
            Observed::Absent,
        )];
        assert_eq!(
            judged(&mut region, &batch(&asserts, &[], &[], &[])),
            Ok(true)
        );
    }

    #[test]
    fn an_assertion_that_has_moved_is_a_conflict_not_an_error() {
        let mut region = Fake::default();
        region.rows.insert(row("a", 1));
        let asserts = [Assertion::index(
            ONE,
            KeyRange::Exact(vec![b"a".to_vec()]),
            None,
            Observed::Absent,
        )];
        assert_eq!(
            judged(&mut region, &batch(&asserts, &[], &[], &[])),
            Ok(false)
        );
    }

    /// The `for_each` case: the reader stopped at its limit, so a row appearing
    /// past that limit is not a conflict.
    #[test]
    fn a_row_past_the_limit_the_reader_stopped_at_is_not_a_conflict() {
        let mut region = Fake::default();
        region.rows.extend([row("a", 1), row("b", 2), row("c", 3)]);
        let asserts = [Assertion::index(
            ONE,
            KeyRange::All,
            Some(2),
            Observed::Rows(vec![row("a", 1), row("b", 2)]),
        )];
        assert_eq!(
            judged(&mut region, &batch(&asserts, &[], &[], &[])),
            Ok(true)
        );
    }

    #[test]
    fn a_object_assertion_reads_presence() {
        let mut region = Fake::default();
        region.objects.insert(ObjectId(1), b"one".to_vec());

        let present = [Assertion::Object {
            id: ObjectId(1),
            present: true,
        }];
        assert_eq!(
            judged(&mut region, &batch(&present, &[], &[], &[])),
            Ok(true)
        );

        let absent = [Assertion::Object {
            id: ObjectId(2),
            present: true,
        }];
        assert_eq!(
            judged(&mut region, &batch(&absent, &[], &[], &[])),
            Ok(false)
        );
    }

    #[test]
    fn a_object_the_store_already_holds_is_refused() {
        let mut region = Fake::default();
        region.objects.insert(ObjectId(1), b"one".to_vec());
        let objects = [(ObjectId(1), b"again".to_vec())];
        assert_eq!(
            judged(&mut region, &batch(&[], &objects, &[], &[])),
            Err(StorageError::DuplicateObject(ObjectId(1)))
        );
    }

    #[test]
    fn an_unknown_index_is_named_from_a_delete_and_from_an_insert() {
        let mut region = Fake::default();
        let delete = [RangeDelete {
            index: UNKNOWN,
            range: KeyRange::All,
        }];
        assert_eq!(
            judged(&mut region, &batch(&[], &[], &delete, &[])),
            Err(StorageError::UnknownIndex(UNKNOWN))
        );

        let insert = [IndexEntry {
            index: UNKNOWN,
            columns: vec![b"a".to_vec()],
            object: ObjectId(1),
        }];
        assert_eq!(
            judged(&mut region, &batch(&[], &[], &[], &insert)),
            Err(StorageError::UnknownIndex(UNKNOWN))
        );
    }

    /// The failure that used to look like success.
    ///
    /// A sweep was a field neither half read, so a store that claimed
    /// `takes_sweep` committed nothing and answered `Applied::Committed`. It
    /// cannot be built into an `Admission` now, so `commit` cannot be reached
    /// with one.
    #[test]
    fn a_store_handed_a_sweep_says_so_rather_than_writing_nothing() {
        let sweep = Sweep {
            index: ONE,
            range: KeyRange::All,
            limit: Some(1),
            template: Vec::new(),
        };
        let mut region = Fake::default();

        let Err(refused) = Admission::over(&mut region, &Batch::Sweep(&sweep)) else {
            panic!("a region cannot write a sweep");
        };
        let StorageError::Engine(detail) = refused else {
            panic!("only the engine can describe this");
        };
        assert!(detail.contains("sweep"), "it names what arrived: {detail}");
    }

    #[test]
    fn a_key_of_the_wrong_width_is_refused() {
        let mut region = Fake::default();
        let insert = [IndexEntry {
            index: ONE,
            columns: vec![b"a".to_vec(), b"b".to_vec()],
            object: ObjectId(1),
        }];
        assert_eq!(
            judged(&mut region, &batch(&[], &[], &[], &insert)),
            Err(StorageError::Arity {
                index: ONE,
                expected: 1,
                found: 2,
            })
        );
    }

    /// A refused batch must not have moved anything, which is only true because
    /// every fallible question is asked before `commit` is called at all.
    #[test]
    fn nothing_is_written_while_the_batch_is_being_judged() {
        let mut region = Fake::default();
        region.objects.insert(ObjectId(1), b"one".to_vec());
        let objects = [(ObjectId(1), b"again".to_vec())];
        let insert = [entry("a", 1)];
        let refused = judged(&mut region, &batch(&[], &objects, &[], &insert));
        assert!(refused.is_err(), "a duplicate object refuses the batch");
        assert!(region.order.is_empty());
        assert!(region.wrote.is_empty());
    }

    #[test]
    fn a_commit_deletes_then_files_objects_then_files_rows() {
        let mut region = Fake::default();
        let objects = [(ObjectId(1), b"one".to_vec())];
        let delete = [RangeDelete {
            index: ONE,
            range: KeyRange::All,
        }];
        let insert = [entry("a", 1), entry("b", 2)];
        written(&mut region, &batch(&[], &objects, &delete, &insert));

        assert_eq!(region.order, ["delete", "object", "row", "row"]);
        assert_eq!(region.wrote, [row("a", 1), row("b", 2)]);
    }
}
