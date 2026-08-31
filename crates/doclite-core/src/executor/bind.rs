//! Binding a template to a row.
//!
//! This is the sharp edge the spec warns about, in one place. A `for_each`
//! body is called once, before any row exists, so what it builds carries row
//! slots rather than bytes. Turning those slots into bytes is the whole of the
//! data dependence this design admits. The other half of the same rule is
//! [`crate::model::column::symbol_allowed`], which refuses a slot where no row
//! will ever exist.

use crate::engine::KeyRange;
use crate::model::column::Column;
use crate::model::encoding::ToOrdBytes;
use crate::model::object::{ObjectId, ObjectRef};
use crate::model::range::{Bound, RangeSpec};
use crate::model::row::Row;
use crate::outcome::error::BuildError;

/// The row a template is being instantiated against, or nothing outside one.
///
/// Every failure here is [`BuildError::RowOutsideTemplate`], never a storage
/// failure: a slot with no row is a defect in the operation, and `check` should
/// have caught it before an engine was ever asked anything.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bind<'a>(Option<&'a Row>);

impl<'a> Bind<'a> {
    /// Outside a template, where every column is already bytes.
    pub(crate) const fn literal() -> Self {
        Self(None)
    }

    /// Inside a template, against the row it is being instantiated for.
    pub(crate) const fn to(row: &'a Row) -> Self {
        Self(Some(row))
    }

    fn row(&self) -> Result<&Row, BuildError> {
        self.0.ok_or(BuildError::RowOutsideTemplate)
    }

    /// One column's bytes.
    pub(crate) fn column(&self, column: &Column) -> Result<Vec<u8>, BuildError> {
        match column {
            Column::Literal(bytes) => Ok(bytes.clone()),
            Column::RowColumn(at) => self
                .row()?
                .key
                .component(*at)
                .map(<[u8]>::to_vec)
                .ok_or(BuildError::RowOutsideTemplate),
            Column::RowObject => Ok(self.row()?.object.to_ord_bytes()),
        }
    }

    /// Which object an index row names.
    pub(crate) fn object(&self, reference: ObjectRef) -> Result<ObjectId, BuildError> {
        match reference {
            ObjectRef::Id(id) => Ok(id),
            ObjectRef::Row => Ok(self.row()?.object),
        }
    }

    /// A range with every component resolved, which is what an engine sees.
    ///
    /// The mapping [`RangeSpec`] to [`KeyRange`] lives here rather than as a
    /// pair of near-identical enums nobody can tell apart: the first may still
    /// hold a slot, the second never does.
    pub(crate) fn range(&self, spec: &RangeSpec) -> Result<KeyRange, BuildError> {
        let columns = |list: &[Column]| {
            list.iter()
                .map(|column| self.column(column))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(match spec {
            RangeSpec::All => KeyRange::All,
            RangeSpec::Exact(list) => KeyRange::Exact(columns(list)?),
            RangeSpec::Prefix(list) => KeyRange::Prefix(columns(list)?),
            RangeSpec::Between { lo, hi } => {
                let endpoint = |side: &Option<(Vec<Column>, Bound)>| match side {
                    None => Ok(None),
                    Some((list, bound)) => columns(list).map(|resolved| Some((resolved, *bound))),
                };
                KeyRange::Between {
                    lo: endpoint(lo)?,
                    hi: endpoint(hi)?,
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::range::Bound;

    fn row() -> Row {
        Row::new(vec![b"clinic".to_vec(), b"2026".to_vec()], ObjectId(7))
    }

    fn literal(value: &str) -> Column {
        Column::Literal(value.as_bytes().to_vec())
    }

    #[test]
    fn a_literal_needs_no_row() {
        let bound = Bind::literal();
        assert_eq!(bound.column(&literal("a")), Ok(b"a".to_vec()));
    }

    #[test]
    fn a_slot_takes_the_column_it_names() {
        let row = row();
        let bound = Bind::to(&row);
        assert_eq!(bound.column(&Column::RowColumn(1)), Ok(b"2026".to_vec()));
    }

    #[test]
    fn a_slot_past_the_end_of_the_row_is_refused() {
        let row = row();
        let bound = Bind::to(&row);
        assert_eq!(
            bound.column(&Column::RowColumn(9)),
            Err(BuildError::RowOutsideTemplate)
        );
    }

    #[test]
    fn the_object_slot_is_the_rows_own_id() {
        let row = row();
        let bound = Bind::to(&row);
        assert_eq!(
            bound.column(&Column::RowObject),
            Ok(ObjectId(7).to_ord_bytes())
        );
        assert_eq!(bound.object(ObjectRef::Row), Ok(ObjectId(7)));
    }

    #[test]
    fn a_literal_id_survives_a_template() {
        let row = row();
        assert_eq!(
            Bind::to(&row).object(ObjectRef::Id(ObjectId(1))),
            Ok(ObjectId(1))
        );
    }

    #[test]
    fn a_range_of_slots_resolves_against_the_row() {
        let row = row();
        let bound = Bind::to(&row);
        let spec = RangeSpec::Between {
            lo: Some((vec![Column::RowColumn(0)], Bound::Incl)),
            hi: Some((vec![Column::RowObject], Bound::Excl)),
        };
        assert_eq!(
            bound.range(&spec),
            Ok(KeyRange::Between {
                lo: Some((vec![b"clinic".to_vec()], Bound::Incl)),
                hi: Some((vec![ObjectId(7).to_ord_bytes()], Bound::Excl)),
            })
        );
    }

    #[test]
    fn an_absent_endpoint_survives_binding() {
        let row = row();
        let spec = RangeSpec::Between {
            lo: Some((vec![Column::RowColumn(0)], Bound::Incl)),
            hi: None,
        };
        assert_eq!(
            Bind::to(&row).range(&spec),
            Ok(KeyRange::Between {
                lo: Some((vec![b"clinic".to_vec()], Bound::Incl)),
                hi: None,
            })
        );
    }

    #[test]
    fn every_slot_is_refused_where_no_row_exists() {
        let bound = Bind::literal();
        assert_eq!(
            bound.column(&Column::RowColumn(0)),
            Err(BuildError::RowOutsideTemplate)
        );
        assert_eq!(
            bound.column(&Column::RowObject),
            Err(BuildError::RowOutsideTemplate)
        );
        assert_eq!(
            bound.object(ObjectRef::Row),
            Err(BuildError::RowOutsideTemplate)
        );
        assert_eq!(
            bound.range(&RangeSpec::Prefix(vec![Column::RowColumn(0)])),
            Err(BuildError::RowOutsideTemplate)
        );
    }
}
