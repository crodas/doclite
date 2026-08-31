//! The structural check on a built operation.
//!
//! What is left of verification once control flow stopped being data: index
//! handles resolve, keys are the width their index is, a `for_each` scan is
//! bounded, and a row slot appears only where a row exists. There is no
//! dataflow analysis here because there is no dataflow.

use crate::model::column::symbol_allowed;
use crate::model::index::{IndexId, IndexSchema};
use crate::model::object::{ObjectId, ObjectRef};
use crate::model::range::RangeSpec;
use crate::operation::step::Step;
use crate::outcome::error::BuildError;
use crate::query::plan::ReadPlan;

/// One catalog, and every question asked against it.
///
/// Every rule here needs the schemas and nothing else, so they are one type
/// rather than six functions each handed the same slice again.
pub struct Check<'a> {
    schemas: &'a [IndexSchema],
}

impl<'a> Check<'a> {
    pub fn against(schemas: &'a [IndexSchema]) -> Self {
        Self { schemas }
    }

    /// Checks a whole step list.
    pub fn steps(&self, list: &[Step]) -> Result<(), BuildError> {
        for step in list {
            self.one(step, false)?;
        }
        Ok(())
    }

    /// Checks a read plan.
    pub fn plan(&self, plan: &ReadPlan) -> Result<(), BuildError> {
        match plan {
            ReadPlan::Object { id } => admits(*id),
            ReadPlan::Count { index, range } => self.range_fits(*index, range, false),
            ReadPlan::Scan { index, range, .. } => self.range_fits(*index, range, false),
            ReadPlan::Zip(left, right) => {
                self.plan(left)?;
                self.plan(right)
            }
        }
    }

    fn one(&self, step: &Step, in_template: bool) -> Result<(), BuildError> {
        match step {
            Step::RequireObject { id, .. } | Step::PutObject { id, .. } => admits(*id),

            Step::Require { index, range, .. } | Step::DeleteIndex { index, range } => {
                self.range_fits(*index, range, in_template)
            }

            Step::PutIndex {
                index,
                columns,
                object,
            } => {
                let arity = self.arity_of(*index)?;
                if columns.len() != arity {
                    return Err(BuildError::Arity {
                        index: *index,
                        expected: arity,
                        found: columns.len(),
                    });
                }
                for column in columns {
                    symbol_allowed(column, in_template)?;
                }
                match object {
                    ObjectRef::Row if !in_template => Err(BuildError::RowOutsideTemplate),
                    // A row's id came out of a scan, so it was admitted on the
                    // way in and there is nothing left to judge.
                    ObjectRef::Row => Ok(()),
                    ObjectRef::Id(id) => admits(*id),
                }
            }

            Step::ForEach { scan, template } => {
                if in_template {
                    return Err(BuildError::NestedForEach);
                }
                if !bounded(scan) {
                    return Err(BuildError::UnboundedForEach);
                }
                self.plan(scan)?;
                for step in template {
                    self.one(step, true)?;
                }
                Ok(())
            }
        }
    }

    fn range_fits(
        &self,
        index: IndexId,
        range: &RangeSpec,
        in_template: bool,
    ) -> Result<(), BuildError> {
        let arity = self.arity_of(index)?;
        let widths = match range {
            RangeSpec::All => vec![0],
            RangeSpec::Exact(columns) => vec![columns.len()],
            RangeSpec::Prefix(columns) => vec![columns.len()],
            RangeSpec::Between { lo, hi } => [lo, hi]
                .iter()
                .filter_map(|side| side.as_ref().map(|(columns, _)| columns.len()))
                .collect(),
        };
        for width in widths {
            if width > arity {
                return Err(BuildError::Arity {
                    index,
                    expected: arity,
                    found: width,
                });
            }
        }
        if let RangeSpec::Exact(columns) = range {
            if columns.len() != arity {
                return Err(BuildError::Arity {
                    index,
                    expected: arity,
                    found: columns.len(),
                });
            }
        }
        for column in range.columns() {
            symbol_allowed(column, in_template)?;
        }
        Ok(())
    }

    fn arity_of(&self, index: IndexId) -> Result<usize, BuildError> {
        self.schemas
            .iter()
            .find(|schema| schema.id == index)
            .map(|schema| schema.arity())
            .ok_or(BuildError::UnknownIndex(index))
    }
}

fn bounded(plan: &ReadPlan) -> bool {
    matches!(plan, ReadPlan::Scan { limit: Some(_), .. })
}

/// Refuses an id that sets the bit [`ObjectId`] reserves.
///
/// Here rather than in an engine, because the promise is the type's and every
/// engine is allowed to lean on it. Refusing at the boundary is what stops an
/// out-of-range id becoming a negative integer that sorts first in a backend
/// whose integers are signed.
fn admits(id: ObjectId) -> Result<(), BuildError> {
    if id.is_valid() {
        return Ok(());
    }
    Err(BuildError::ObjectIdTooLarge(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::column::Column;
    use crate::model::index::ColumnType;
    use crate::model::object::ObjectId;
    use crate::outcome::abort::AbortCode;
    use crate::query::plan::ObjectMode;

    static ONE: IndexId = IndexId("one");

    fn schemas() -> Vec<IndexSchema> {
        vec![IndexSchema {
            id: ONE,
            columns: &[ColumnType::Bytes],
        }]
    }

    fn scan(limit: Option<usize>) -> ReadPlan {
        ReadPlan::Scan {
            index: ONE,
            range: RangeSpec::All,
            limit,
            objects: ObjectMode::None,
        }
    }

    fn put(columns: Vec<Column>, object: ObjectRef) -> Step {
        Step::PutIndex {
            index: ONE,
            columns,
            object,
        }
    }

    fn check(steps: &[Step]) -> Result<(), BuildError> {
        Check::against(&schemas()).steps(steps)
    }

    #[test]
    fn a_step_naming_an_index_nobody_declared_is_refused() {
        let steps = [Step::DeleteIndex {
            index: IndexId("nobody_declared_this"),
            range: RangeSpec::All,
        }];
        assert_eq!(
            check(&steps),
            Err(BuildError::UnknownIndex(IndexId("nobody_declared_this")))
        );
    }

    #[test]
    fn a_key_wider_than_its_index_is_refused() {
        let steps = [put(
            vec![
                Column::Literal(b"a".to_vec()),
                Column::Literal(b"b".to_vec()),
            ],
            ObjectRef::Id(ObjectId(1)),
        )];
        assert_eq!(
            check(&steps),
            Err(BuildError::Arity {
                index: ONE,
                expected: 1,
                found: 2,
            })
        );
    }

    /// A slot names the row a template is being instantiated for, so outside
    /// one there is no row for it to mean.
    #[test]
    fn a_row_slot_outside_a_template_is_refused() {
        let steps = [put(vec![Column::RowColumn(0)], ObjectRef::Id(ObjectId(1)))];
        assert_eq!(check(&steps), Err(BuildError::RowOutsideTemplate));

        let steps = [Step::ForEach {
            scan: Box::new(scan(Some(10))),
            template: vec![put(vec![Column::RowColumn(0)], ObjectRef::Row)],
        }];
        assert_eq!(check(&steps), Ok(()), "inside one there is a row");
    }

    /// The one step that can do unbounded work, so its scan carries a limit or
    /// the operation never runs.
    #[test]
    fn a_for_each_whose_scan_has_no_limit_is_refused() {
        let steps = [Step::ForEach {
            scan: Box::new(scan(None)),
            template: Vec::new(),
        }];
        assert_eq!(check(&steps), Err(BuildError::UnboundedForEach));
    }

    #[test]
    fn a_for_each_inside_a_for_each_is_refused() {
        let inner = Step::ForEach {
            scan: Box::new(scan(Some(10))),
            template: Vec::new(),
        };
        let steps = [Step::ForEach {
            scan: Box::new(scan(Some(10))),
            template: vec![inner],
        }];
        assert_eq!(check(&steps), Err(BuildError::NestedForEach));
    }

    /// A read plan is checked the same way, so a query naming an index nobody
    /// declared is refused before an engine is asked about it.
    #[test]
    fn a_plan_is_checked_against_the_same_catalog() {
        let unknown = ReadPlan::Count {
            index: IndexId("nobody_declared_this"),
            range: RangeSpec::All,
        };
        assert_eq!(
            Check::against(&schemas()).plan(&unknown),
            Err(BuildError::UnknownIndex(IndexId("nobody_declared_this")))
        );
    }

    /// The largest id an operation may name, and the first one it may not.
    ///
    /// Refusing here is what lets a backend with signed integers hold an id as
    /// an integer. Without it the id below would arrive negative and sort
    /// first, and index order is not a hint.
    #[test]
    fn an_id_that_sets_the_reserved_bit_is_refused() {
        let too_large = ObjectId(ObjectId::LIMIT);
        let largest = ObjectId(ObjectId::LIMIT - 1);

        for step in [
            Step::PutObject {
                id: too_large,
                bytes: Vec::new(),
            },
            Step::RequireObject {
                id: too_large,
                code: AbortCode::new(1),
            },
            put(vec![Column::encode("a")], ObjectRef::Id(too_large)),
        ] {
            assert_eq!(
                check(&[step]),
                Err(BuildError::ObjectIdTooLarge(too_large)),
                "every step carrying a literal id is checked"
            );
        }

        assert_eq!(
            Check::against(&schemas()).plan(&ReadPlan::Object { id: too_large }),
            Err(BuildError::ObjectIdTooLarge(too_large)),
            "a read naming one is refused too"
        );

        assert_eq!(
            check(&[Step::PutObject {
                id: largest,
                bytes: Vec::new(),
            }]),
            Ok(()),
            "the value one below the limit is ordinary"
        );
    }
}
