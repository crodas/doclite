//! A transaction's worth of work, built as a value.

pub(crate) mod step;
pub(crate) mod template;

use crate::check::Check;
use crate::model::column::IntoObject;
use crate::model::index::{Index, IndexId, IndexSchema};
use crate::model::key::Key;
use crate::model::object::ObjectId;
use crate::model::range::Range;
use crate::model::row::Row;
use crate::model::storage::ToStorage;
use crate::operation::step::{Expect, Step};
use crate::operation::template::RowRef;
use crate::outcome::abort::AbortCode;
use crate::outcome::error::BuildError;
use crate::query::plan::named_once;
use crate::query::Query;

/// Work to be committed or refused, whole.
///
/// There is no `begin`, no `commit` and no `rollback`, because an operation
/// *is* a transaction and [`Db::run`](crate::Db::run) is the boundary. An
/// unbalanced transaction is not something you can construct.
///
/// Steps are one ordered list, so preconditions and mutations interleave in
/// written order and a precondition placed after a mutation observes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation<T = ()> {
    steps: Vec<Step>,
    /// The first step that could not be built, kept rather than returned so the
    /// chain stays one expression. [`Operation::check`] is where it surfaces,
    /// and [`Db::run`](crate::Db::run) checks before it touches an engine.
    error: Option<BuildError>,
    value: T,
}

/// The indexes one step reads, appended in the order they are named.
fn read_by(step: &Step, into: &mut Vec<IndexId>) {
    match step {
        Step::RequireObject { .. }
        | Step::PutObject { .. }
        | Step::PutIndex { .. }
        | Step::DeleteIndex { .. } => {}
        Step::Require { index, .. } => named_once(*index, into),
        Step::ForEach { scan, template } => {
            for index in scan.reads() {
                named_once(index, into);
            }
            for step in template {
                read_by(step, into);
            }
        }
    }
}

impl Default for Operation<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl Operation<()> {
    /// An operation with no steps, which commits and changes nothing.
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            error: None,
            value: (),
        }
    }
}

impl<T> Operation<T> {
    /// The steps in the order they will run, which is what lets a caller assert
    /// what an operation contains without an engine to run it against.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Whether running this changes anything.
    pub fn mutates(&self) -> bool {
        self.steps.iter().any(Step::mutates)
    }

    /// Every index this operation reads, which is not every index it names.
    ///
    /// Filing a row into an index whose rows are not all there is safe: the row
    /// belongs there either way, and the fill will not file it twice. Retiring a
    /// range is safe for the same reason. **Reading** one is not, and a
    /// precondition is a read: `require_absent` on an index with rows missing
    /// finds a key absent that another document already holds, so two documents
    /// claim one unique key and nothing afterwards can tell. That asymmetry is
    /// the whole reason this is `reads` rather than `indexes`, and it is why
    /// `PutIndex` and `DeleteIndex` are deliberately absent below.
    pub fn reads(&self) -> Vec<IndexId> {
        let mut named = Vec::new();
        for step in &self.steps {
            read_by(step, &mut named);
        }
        named
    }

    /// Whether index handles resolve, keys are the width their index is, a
    /// `for_each` scan is bounded, and a row slot appears only where a row
    /// exists.
    ///
    /// Calling this is optional. [`Db::run`](crate::Db::run) checks the same
    /// things before it touches an engine.
    pub fn check(&self, schemas: &[IndexSchema]) -> Result<(), BuildError> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Check::against(schemas).steps(&self.steps)
    }

    fn push(mut self, step: Step) -> Self {
        self.steps.push(step);
        self
    }

    /// Refuse unless the range holds nothing.
    pub fn require_absent<const N: usize, const K: usize>(
        self,
        index: &Index<N>,
        range: Range<N, K>,
        code: AbortCode,
    ) -> Self {
        self.push(Step::Require {
            index: index.id(),
            range: range.into_spec(),
            expect: Expect::Absent,
            code,
        })
    }

    /// Refuse unless the range holds exactly this many rows.
    pub fn require_count<const N: usize, const K: usize>(
        self,
        index: &Index<N>,
        range: Range<N, K>,
        rows: u64,
        code: AbortCode,
    ) -> Self {
        self.push(Step::Require {
            index: index.id(),
            range: range.into_spec(),
            expect: Expect::Count(rows),
            code,
        })
    }

    /// Refuse unless the object is there.
    pub fn require_object(self, id: ObjectId, code: AbortCode) -> Self {
        self.push(Step::RequireObject { id, code })
    }

    /// File an object. The object store is additive, so this is never an
    /// update.
    ///
    /// The value is encoded here rather than at run time, so a type that cannot
    /// be encoded is a defect in the operation. The failure is kept until
    /// [`Operation::check`] rather than returned, so the chain stays one
    /// expression.
    pub fn put_object(mut self, id: ObjectId, value: impl ToStorage) -> Self {
        match value.to_storage() {
            Ok(bytes) => self.push(Step::PutObject { id, bytes }),
            Err(error) => {
                self.error.get_or_insert(BuildError::Encode(error));
                self
            }
        }
    }

    /// File an index row.
    pub fn put_index<const N: usize>(
        self,
        index: &Index<N>,
        key: Key<N>,
        object: impl IntoObject,
    ) -> Self {
        self.push(Step::PutIndex {
            index: index.id(),
            columns: key.into_columns(),
            object: object.into_object(),
        })
    }

    /// Retire every index row in the range.
    pub fn delete_index<const N: usize, const K: usize>(
        self,
        index: &Index<N>,
        range: Range<N, K>,
    ) -> Self {
        self.push(Step::DeleteIndex {
            index: index.id(),
            range: range.into_spec(),
        })
    }

    /// Run a template once per scanned row.
    ///
    /// The body is called **once, at build time**, with a symbolic row. What
    /// comes back is a template the engine instantiates per row. This is the
    /// one place the design admits real data dependence, and the one
    /// combinator that can do unbounded work, so the scan must carry a
    /// `.limit()`.
    pub fn for_each<F>(self, scan: Query<Vec<Row>>, body: F) -> Self
    where
        F: FnOnce(RowRef) -> Operation<()>,
    {
        let template = body(RowRef);
        self.push(Step::ForEach {
            scan: Box::new(scan.plan().clone()),
            template: template.steps,
        })
    }

    /// What a committed run hands back.
    pub fn returning<U>(self, value: U) -> Operation<U> {
        Operation {
            steps: self.steps,
            error: self.error,
            value,
        }
    }

    /// Two operations, still one transaction.
    ///
    /// This is what replaces the delimiters: a boundary you can pass around
    /// and combine rather than a pair of instructions to get wrong.
    pub fn then<U>(mut self, other: Operation<U>) -> Operation<U> {
        self.steps.extend(other.steps);
        Operation {
            steps: self.steps,
            error: self.error.or(other.error),
            value: other.value,
        }
    }
}

impl<T: Clone> Operation<T> {
    pub(crate) fn value(&self) -> T {
        self.value.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::index::ColumnType;
    use crate::model::range::{all, exact};
    use crate::{key, ObjectId};

    static PLAIN: Index<1> = Index::typed("plain", &[ColumnType::Str]);
    static OTHER: Index<1> = Index::typed("other", &[ColumnType::Str]);
    const REFUSED: AbortCode = AbortCode::new(900);

    /// Filing a row into an index whose rows are not all there is safe, so it
    /// must not make the caller wait: the row belongs in that index either way,
    /// and a fill that is still running will not file it twice.
    #[test]
    fn filing_a_row_is_not_a_read_of_the_index_it_goes_in() {
        let op = Operation::new().put_index(&PLAIN, key!["a"], ObjectId(7));
        assert_eq!(op.reads(), Vec::new());
    }

    /// Retiring a range is safe for the same reason: it either removes a row a
    /// fill already filed or matches nothing at all.
    #[test]
    fn retiring_a_range_is_not_a_read_either() {
        let op = Operation::new().delete_index(&PLAIN, exact(key!["a"]));
        assert_eq!(op.reads(), Vec::new());
    }

    /// The one that matters. `require_absent` on an index with rows missing
    /// finds a key absent that another document already holds, so two
    /// documents claim one unique key and nothing afterwards can tell them
    /// apart. A unique claim has to wait.
    #[test]
    fn claiming_a_unique_key_is_a_read() {
        let op = Operation::new().require_absent(&PLAIN, exact(key!["a"]), REFUSED);
        assert_eq!(op.reads(), vec![PLAIN.id()]);
    }

    #[test]
    fn counting_a_range_is_a_read() {
        let op = Operation::new().require_count(&PLAIN, all(), 1, REFUSED);
        assert_eq!(op.reads(), vec![PLAIN.id()]);
    }

    /// A write is a read of what it asserted and nothing more, which is what
    /// keeps an insert from waiting on the index it is only filing into.
    #[test]
    fn a_write_waits_for_what_it_asserts_and_not_for_what_it_files() {
        let op = Operation::new()
            .require_absent(&PLAIN, exact(key!["a"]), REFUSED)
            .put_object(ObjectId(7), b"body".to_vec())
            .put_index(&PLAIN, key!["a"], ObjectId(7))
            .put_index(&OTHER, key!["a"], ObjectId(7));
        assert_eq!(op.reads(), vec![PLAIN.id()], "only the claim is a read");
    }

    #[test]
    fn a_for_each_reads_the_index_it_scans() {
        let scan = Query::scan(&OTHER, all()).limit(10);
        let op = Operation::new().for_each(scan, |row| {
            Operation::new().put_index(&PLAIN, key![row.key(0)], row.doc())
        });
        assert_eq!(op.reads(), vec![OTHER.id()]);
    }

    #[test]
    fn an_index_read_twice_is_waited_for_once() {
        let op = Operation::new()
            .require_absent(&PLAIN, exact(key!["a"]), REFUSED)
            .require_count(&PLAIN, all(), 0, REFUSED);
        assert_eq!(op.reads(), vec![PLAIN.id()]);
    }
}
