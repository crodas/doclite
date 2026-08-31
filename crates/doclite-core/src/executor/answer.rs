//! Answering a query: one plan, one call to the engine, one shaped value.
//!
//! The plan tree is flattened so every read of a query goes out together, which
//! is what makes `Query::zip` agree with itself, and the answers are put back
//! into the plan's shape afterwards.

use crate::db::Db;
use crate::engine::KeyRange;
use crate::engine::{Answers, Engine, IndexRead, Plan, Reads};
use crate::executor::bind::Bind;
use crate::model::index::IndexId;
use crate::model::object::ObjectId;
use crate::model::row::Row;
use crate::outcome::abort::AbortCode;
use crate::outcome::error::StorageError;
use crate::outcome::Outcome;
use crate::query::plan::{ObjectMode, ReadPlan};
use crate::query::value::ReadValue;

/// The whole read side of one query.
///
/// The engine is offered the plan whole before it is taken apart, because a
/// backend with a planner can answer a count where the rows are and a zip in
/// one statement. Declining is the default and costs nothing: what follows is
/// the same path an engine that never planned anything has always taken.
pub(crate) async fn answer<E: Engine>(
    db: &Db<E>,
    plan: &ReadPlan,
) -> Result<Outcome<ReadValue>, StorageError> {
    Reading { db }.answer(plan).await
}

/// One query being answered, against the store it is answered from.
///
/// Everything needing the engine is a method here. What is pure over the plan
/// stays a function below, because holding a db would collect nothing.
struct Reading<'a, E> {
    db: &'a Db<E>,
}

impl<E: Engine> Reading<'_, E> {
    async fn answer(&self, plan: &ReadPlan) -> Result<Outcome<ReadValue>, StorageError> {
        let shaped = match self.planned(plan).await? {
            Some(value) => value,
            None => {
                let mut slots = Vec::new();
                flatten(plan, &mut slots)?;
                let values = self.issue(&slots).await?;
                rebuild(plan, &mut values.into_iter())?
            }
        };

        if dangling(plan, &shaped) {
            return Ok(Outcome::Aborted(AbortCode::DANGLING_INDEX_ENTRY));
        }
        Ok(Outcome::Committed(shaped))
    }

    /// Offers the plan to the engine with every range resolved.
    ///
    /// A range in a plan is a spec that may still name a template slot, and
    /// resolving one is not an engine's business, so it is done here or the
    /// offer is not made.
    async fn planned(&self, plan: &ReadPlan) -> Result<Option<ReadValue>, StorageError> {
        self.db.engine().plan(&resolve(plan)?).await
    }

    /// Every read of a query in one call, which is what makes `zip` agree with
    /// itself.
    ///
    /// A scan that also wants objects needs a second call, because which
    /// objects to fetch is not known until the first one answers. Only those
    /// payloads sit outside the shared view, and a row whose object went missing
    /// in between is the dangling case, which already has an answer.
    async fn issue(&self, slots: &[Slot]) -> Result<Vec<ReadValue>, StorageError> {
        let scans: Vec<IndexRead<'_>> = slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Count { index, range } => Some(IndexRead::new(*index, range)),
                Slot::Scan {
                    index,
                    range,
                    limit,
                    ..
                } => Some(IndexRead {
                    index: *index,
                    range,
                    limit: *limit,
                }),
                Slot::Object { .. } => None,
            })
            .collect();

        let objects: Vec<ObjectId> = slots
            .iter()
            .filter_map(|slot| match slot {
                Slot::Object { id } => Some(*id),
                Slot::Count { .. } | Slot::Scan { .. } => None,
            })
            .collect();

        let answers = self
            .db
            .engine()
            .read(&Reads {
                scans: &scans,
                objects: &objects,
            })
            .await?;
        tally(&answers, scans.len(), objects.len())?;

        let mut values: Vec<Option<ReadValue>> = vec![None; slots.len()];

        let asked_scans = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !matches!(slot, Slot::Object { .. }));
        for ((at, slot), rows) in asked_scans.zip(answers.scans) {
            values[at] = Some(self.shape(slot, rows).await?);
        }

        let asked_objects = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| matches!(slot, Slot::Object { .. }));
        for ((at, _), body) in asked_objects.zip(answers.objects) {
            values[at] = Some(ReadValue::Object(body));
        }

        values
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(StorageError::ReadMismatch)
    }

    /// Turns one scan's rows into the value its slot promised.
    async fn shape(&self, slot: &Slot, rows: Vec<Row>) -> Result<ReadValue, StorageError> {
        match slot {
            Slot::Count { range, .. } => Ok(ReadValue::Count(range.select(rows).len() as u64)),

            Slot::Scan {
                range,
                limit,
                objects,
                ..
            } => {
                let mut rows = range.select(rows);
                if let Some(limit) = limit {
                    rows.truncate(*limit);
                }
                match objects {
                    ObjectMode::None => Ok(ReadValue::Rows(rows)),
                    ObjectMode::Strict | ObjectMode::Lenient => {
                        let bodies = self.objects_for(&rows).await?;
                        Ok(ReadValue::RowsWithObjects(
                            rows.into_iter().zip(bodies).collect(),
                        ))
                    }
                }
            }

            Slot::Object { .. } => Err(StorageError::ReadMismatch),
        }
    }

    async fn objects_for(&self, rows: &[Row]) -> Result<Vec<Option<Vec<u8>>>, StorageError> {
        let ids: Vec<ObjectId> = rows.iter().map(|row| row.object).collect();
        let answers = self.db.engine().read(&Reads::objects(&ids)).await?;
        tally(&answers, 0, ids.len())?;
        Ok(answers.objects)
    }
}

/// The same plan with each range bound to literal columns.
fn resolve(plan: &ReadPlan) -> Result<Plan, StorageError> {
    Ok(match plan {
        ReadPlan::Count { index, range } => Plan::Count {
            index: *index,
            range: Bind::literal().range(range)?,
        },
        ReadPlan::Object { id } => Plan::Object { id: *id },
        ReadPlan::Scan {
            index,
            range,
            limit,
            objects,
        } => Plan::Rows {
            index: *index,
            range: Bind::literal().range(range)?,
            limit: *limit,
            objects: *objects,
        },
        ReadPlan::Zip(left, right) => {
            Plan::Zip(Box::new(resolve(left)?), Box::new(resolve(right)?))
        }
    })
}

/// One read of a query, flattened out of the plan tree.
enum Slot {
    Count {
        index: IndexId,
        range: KeyRange,
    },
    Scan {
        index: IndexId,
        range: KeyRange,
        limit: Option<usize>,
        objects: ObjectMode,
    },
    Object {
        id: ObjectId,
    },
}

fn flatten(plan: &ReadPlan, slots: &mut Vec<Slot>) -> Result<(), StorageError> {
    match plan {
        ReadPlan::Count { index, range } => slots.push(Slot::Count {
            index: *index,
            range: Bind::literal().range(range)?,
        }),
        ReadPlan::Scan {
            index,
            range,
            limit,
            objects,
        } => slots.push(Slot::Scan {
            index: *index,
            range: Bind::literal().range(range)?,
            limit: *limit,
            objects: *objects,
        }),
        ReadPlan::Object { id } => slots.push(Slot::Object { id: *id }),
        ReadPlan::Zip(left, right) => {
            flatten(left, slots)?;
            flatten(right, slots)?;
        }
    }
    Ok(())
}

/// An engine owes one answer per scan and one per object id.
fn tally(answers: &Answers, scans: usize, objects: usize) -> Result<(), StorageError> {
    if answers.scans.len() != scans {
        return Err(StorageError::MissingAnswers {
            asked: scans,
            answered: answers.scans.len(),
        });
    }
    if answers.objects.len() != objects {
        return Err(StorageError::MissingAnswers {
            asked: objects,
            answered: answers.objects.len(),
        });
    }
    Ok(())
}

/// Puts the flat answers back into the shape the plan asked for.
fn rebuild(
    plan: &ReadPlan,
    values: &mut impl Iterator<Item = ReadValue>,
) -> Result<ReadValue, StorageError> {
    match plan {
        ReadPlan::Count { .. } | ReadPlan::Scan { .. } | ReadPlan::Object { .. } => {
            values.next().ok_or(StorageError::ReadMismatch)
        }
        ReadPlan::Zip(left, right) => {
            let a = rebuild(left, values)?;
            let b = rebuild(right, values)?;
            Ok(ReadValue::Pair(Box::new(a), Box::new(b)))
        }
    }
}

/// Whether any strict scan came back naming an object that is not there.
fn dangling(plan: &ReadPlan, value: &ReadValue) -> bool {
    match (plan, value) {
        (
            ReadPlan::Scan {
                objects: ObjectMode::Strict,
                ..
            },
            ReadValue::RowsWithObjects(rows),
        ) => rows.iter().any(|(_, body)| body.is_none()),
        (ReadPlan::Zip(left, right), ReadValue::Pair(a, b)) => {
            dangling(left, a) || dangling(right, b)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::Fake;
    use crate::model::index::{ColumnType, Index};
    use crate::model::range::all;
    use crate::model::row::Row;
    use crate::query::Query;

    static ONE: Index<1> = Index::typed("one", &[ColumnType::Bytes]);

    async fn open(engine: Fake) -> Db<Fake> {
        Db::open(engine, &[ONE.schema()])
            .await
            .expect("the fake accepts its own schemas")
    }

    fn rows() -> Vec<Row> {
        vec![Row::new(vec![b"a".to_vec()], ObjectId(1))]
    }

    /// Answering the plan is what a backend with a planner is for: the count is
    /// the engine's, and the rows never move to be counted here.
    #[tokio::test]
    async fn a_planning_engine_answers_and_nothing_is_scanned() {
        let engine = Fake::declining()
            .holding(ONE.schema().id, rows())
            .planning(ReadValue::Count(41));
        let db = open(engine).await;

        let count = db
            .run(&Query::count(&ONE, all::<1>()))
            .await
            .expect("the fake answers")
            .committed()
            .expect("a count is not something an assertion can refuse");
        assert_eq!(count, 41, "the engine's answer, not the rows counted here");
        assert_eq!(db.engine().offered().scans, 0);
    }

    /// Declining is the default and costs nothing: the read happens the
    /// ordinary way, through flat scans.
    #[tokio::test]
    async fn a_declining_engine_is_still_offered_the_plan_and_then_scanned() {
        let engine = Fake::declining().holding(ONE.schema().id, rows());
        let db = open(engine).await;

        let count = db
            .run(&Query::count(&ONE, all::<1>()))
            .await
            .expect("the fake answers")
            .committed()
            .expect("a count is not something an assertion can refuse");
        assert_eq!(count, 1, "counted from the rows that came back");

        let offered = db.engine().offered();
        assert_eq!(
            offered.plans.len(),
            1,
            "it was offered before being flattened"
        );
        assert_eq!(offered.scans, 1);
    }
}
