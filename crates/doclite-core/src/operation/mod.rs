//! A transaction's worth of work, built as a value.

pub(crate) mod step;
pub(crate) mod template;

use crate::check::Check;
use crate::model::column::IntoObject;
use crate::model::index::{Index, IndexSchema};
use crate::model::key::Key;
use crate::model::object::ObjectId;
use crate::model::range::Range;
use crate::model::row::Row;
use crate::model::storage::ToStorage;
use crate::operation::step::{Expect, Step};
use crate::operation::template::RowRef;
use crate::outcome::abort::AbortCode;
use crate::outcome::error::BuildError;
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
