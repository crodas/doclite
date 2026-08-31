//! Walking an operation once, against the overlay it is building.
//!
//! Every precondition reads the engine, folds the staged mutations over the
//! answer, and records what it saw as an assertion. Nothing reaches the engine
//! until the whole list has run.

use crate::db::Db;
use crate::engine::KeyRange;
use crate::engine::{
    Applied, Assertion, Batch, Engine, IndexEntry, IndexRead, Observed, RangeDelete, Reads, Sweep,
};
use crate::executor::bind::Bind;
use crate::executor::delta::Delta;
use crate::model::index::IndexId;
use crate::model::object::ObjectId;
use crate::model::row::Row;
use crate::operation::step::{Expect, Step};
use crate::outcome::abort::AbortCode;
use crate::outcome::error::{BuildError, StorageError};
use crate::query::plan::ReadPlan;

pub(crate) enum Attempt {
    Committed,
    Aborted(AbortCode),
    Conflict,
}

pub(crate) async fn walk<E: Engine>(db: &Db<E>, steps: &[Step]) -> Result<Attempt, StorageError> {
    Pass::over(db).run(steps).await
}

/// One pass over an operation's steps, and what it staged.
///
/// A pass is what `Db::run` repeats when an engine reports a conflict, which is
/// why it is a value rather than three things handed to each other: the delta
/// and the assertions belong to this attempt and are discarded with it.
struct Pass<'a, E> {
    db: &'a Db<E>,
    delta: Delta,
    asserts: Vec<Assertion>,
}

impl<'a, E: Engine> Pass<'a, E> {
    fn over(db: &'a Db<E>) -> Self {
        Self {
            db,
            delta: Delta::default(),
            asserts: Vec::new(),
        }
    }

    async fn run(mut self, steps: &[Step]) -> Result<Attempt, StorageError> {
        if let Some(sweep) = self.swept(steps).await? {
            return self.sweep(&sweep).await;
        }

        for step in steps {
            match step {
                Step::ForEach { scan, template } => {
                    let (index, range, limit) = scan_parts(scan)?;
                    let rows = self.scan_engine(index, &range, limit).await?;
                    self.asserts.push(Assertion::index(
                        index,
                        range,
                        limit,
                        Observed::Rows(rows.clone()),
                    ));
                    for row in &rows {
                        for step in template {
                            if let Some(code) = self.step(step, Bind::to(row)).await? {
                                return Ok(Attempt::Aborted(code));
                            }
                        }
                    }
                }
                other => {
                    if let Some(code) = self.step(other, Bind::literal()).await? {
                        return Ok(Attempt::Aborted(code));
                    }
                }
            }
        }

        self.commit().await
    }
}

/// The sweep this operation is, if the engine will take it.
///
/// Only a lone `for_each` is offered. Expanding a template stages into the
/// overlay a later precondition folds, so deferring one with a step after it
/// would change what that step observes rather than only how the work was done.
/// With one step there is nothing after it, nothing before it staged anything
/// its scan could have missed, and the scan and its writes land in one batch, so
/// the assertion the expansion would have recorded has nothing left to protect.
/// That is why `Batch::Sweep` carries no assertions: there are none to carry.
impl<E: Engine> Pass<'_, E> {
    async fn swept(&self, steps: &[Step]) -> Result<Option<Sweep>, StorageError> {
        let [Step::ForEach { scan, template }] = steps else {
            return Ok(None);
        };
        let (index, range, limit) = scan_parts(scan)?;
        let sweep = Sweep {
            index,
            range,
            limit,
            template: template.clone(),
        };
        Ok(self.db.engine().takes_sweep(&sweep).await.then_some(sweep))
    }
}

/// Hands the staged work to the engine, or answers without it when nothing was
/// staged at all.
impl<E: Engine> Pass<'_, E> {
    /// A sweep is the whole operation, so it never touches the delta: there is
    /// nothing staged for it to sit beside and no assertion to carry.
    async fn sweep(&self, sweep: &Sweep) -> Result<Attempt, StorageError> {
        match self.db.engine().apply(&Batch::Sweep(sweep)).await? {
            Applied::Committed => Ok(Attempt::Committed),
            Applied::Conflict => Ok(Attempt::Conflict),
        }
    }

    async fn commit(self) -> Result<Attempt, StorageError> {
        if self.delta.is_empty() {
            return Ok(Attempt::Committed);
        }

        let staged = self.delta.into_batch();
        match self.db.engine().apply(&staged.batch(&self.asserts)).await? {
            Applied::Committed => Ok(Attempt::Committed),
            Applied::Conflict => Ok(Attempt::Conflict),
        }
    }
}

impl<E: Engine> Pass<'_, E> {
    /// Runs one step, returning the abort code if a precondition refused.
    async fn step(
        &mut self,
        step: &Step,
        bind: Bind<'_>,
    ) -> Result<Option<AbortCode>, StorageError> {
        match step {
            Step::Require {
                index,
                range,
                expect,
                code,
            } => {
                let range = bind.range(range)?;
                let committed = self.scan_engine(*index, &range, None).await?;
                let observed = Observed::recorded(expect, self.delta.touches(*index), &committed);
                let visible = self.delta.rows_in(*index, &range, committed);
                let holds = match expect {
                    Expect::Absent => visible.is_empty(),
                    Expect::Count(wanted) => visible.len() as u64 == *wanted,
                };
                self.asserts
                    .push(Assertion::index(*index, range, None, observed));
                Ok((!holds).then_some(*code))
            }

            Step::RequireObject { id, code } => {
                let committed = self.fetch_object(*id).await?;
                self.asserts.push(Assertion::Object {
                    id: *id,
                    present: committed.is_some(),
                });
                let visible = committed.is_some() || self.delta.staged_object(*id).is_some();
                Ok((!visible).then_some(*code))
            }

            Step::PutObject { id, bytes } => {
                self.delta.put_object(*id, bytes.clone());
                Ok(None)
            }

            Step::PutIndex {
                index,
                columns,
                object,
            } => {
                let columns = columns
                    .iter()
                    .map(|column| bind.column(column))
                    .collect::<Result<Vec<_>, _>>()?;
                self.delta.insert(IndexEntry {
                    index: *index,
                    columns,
                    object: bind.object(*object)?,
                });
                Ok(None)
            }

            Step::DeleteIndex { index, range } => {
                self.delta.delete(RangeDelete {
                    index: *index,
                    range: bind.range(range)?,
                });
                Ok(None)
            }

            Step::ForEach { .. } => Err(StorageError::Invalid(BuildError::NestedForEach)),
        }
    }

    /// One scan, filtered and trimmed to what was asked for.
    ///
    /// The range and the limit are hints, so what comes back is filtered here
    /// rather than trusted. Filtering before trimming is the order that matters:
    /// the other way round can answer with fewer rows than the limit while more
    /// still match.
    async fn scan_engine(
        &self,
        index: IndexId,
        range: &KeyRange,
        limit: Option<usize>,
    ) -> Result<Vec<Row>, StorageError> {
        let scans = [IndexRead {
            index,
            range,
            limit,
        }];
        let mut answers = self.db.engine().read(&Reads::scans(&scans)).await?;
        let rows = only(answers.scans.pop(), 1, answers.scans.len())?;
        let mut rows = range.select(rows);
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        Ok(rows)
    }

    async fn fetch_object(&self, id: ObjectId) -> Result<Option<Vec<u8>>, StorageError> {
        let wanted = [id];
        let mut answers = self.db.engine().read(&Reads::objects(&wanted)).await?;
        only(answers.objects.pop(), 1, answers.objects.len())
    }
}

/// The single answer a one-element read was owed.
fn only<T>(answer: Option<T>, asked: usize, answered: usize) -> Result<T, StorageError> {
    answer.ok_or(StorageError::MissingAnswers { asked, answered })
}

fn scan_parts(plan: &ReadPlan) -> Result<(IndexId, KeyRange, Option<usize>), StorageError> {
    match plan {
        ReadPlan::Scan {
            index,
            range,
            limit,
            ..
        } => Ok((*index, Bind::literal().range(range)?, *limit)),
        ReadPlan::Count { .. } | ReadPlan::Object { .. } | ReadPlan::Zip(_, _) => {
            Err(StorageError::Invalid(BuildError::UnboundedForEach))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::Fake;
    use crate::key;
    use crate::model::index::{ColumnType, Index};
    use crate::model::range::{all, exact};
    use crate::operation::Operation;
    use crate::query::Query;

    static ONE: Index<1> = Index::typed("one", &[ColumnType::Bytes]);
    static TWO: Index<1> = Index::typed("two", &[ColumnType::Bytes]);

    async fn open(engine: Fake) -> Db<Fake> {
        Db::open(engine, &[ONE.schema(), TWO.schema()])
            .await
            .expect("the fake accepts its own schemas")
    }

    fn cascade() -> Operation<()> {
        Operation::new().for_each(Query::scan(&ONE, all::<1>()).limit(10), |row| {
            Operation::new().delete_index(&TWO, exact(key![row.key(0)]))
        })
    }

    /// The sweep an engine is offered is the `for_each` unexpanded, and a lone
    /// one is the only shape offered at all.
    #[tokio::test]
    async fn a_lone_for_each_is_offered_to_an_engine_that_takes_it() {
        let db = open(Fake::declining().taking_sweeps()).await;
        assert!(db
            .run(&cascade())
            .await
            .expect("the fake commits")
            .is_committed());

        let offered = db.engine().offered();
        assert_eq!(offered.sweeps.len(), 1, "the sweep reached apply");
        assert_eq!(offered.sweeps[0].index, ONE.schema().id);
        assert_eq!(offered.sweeps[0].limit, Some(10));
        assert_eq!(offered.scans, 0, "no row was read to expand it");
    }

    /// Declining costs nothing: the executor expands the template itself, which
    /// means reading the rows.
    #[tokio::test]
    async fn a_declining_engine_gets_the_expansion_instead() {
        let engine = Fake::declining().holding(
            ONE.schema().id,
            vec![Row::new(vec![b"a".to_vec()], ObjectId(1))],
        );
        let db = open(engine).await;
        assert!(db
            .run(&cascade())
            .await
            .expect("the fake commits")
            .is_committed());

        let offered = db.engine().offered();
        assert!(offered.sweeps.is_empty(), "nothing was swept");
        assert_eq!(
            offered.scans, 1,
            "the rows were read to expand the template"
        );
    }

    /// A `for_each` with a step after it is not offered, because expanding a
    /// template stages into the overlay that later step would read.
    #[tokio::test]
    async fn a_for_each_with_a_step_after_it_is_not_offered() {
        let db = open(Fake::declining().taking_sweeps()).await;
        let with_a_tail = Operation::new()
            .for_each(Query::scan(&ONE, all::<1>()).limit(10), |row| {
                Operation::new().delete_index(&TWO, exact(key![row.key(0)]))
            })
            .put_index(&TWO, key!["after"], ObjectId(9));
        assert!(db
            .run(&with_a_tail)
            .await
            .expect("the fake commits")
            .is_committed());

        assert!(
            db.engine().offered().sweeps.is_empty(),
            "a sweep is only ever the whole operation"
        );
    }
}
