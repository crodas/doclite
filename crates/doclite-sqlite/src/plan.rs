//! Answering a read in the shape it was asked, rather than as flat scans.
//!
//! This is what the wide seam was for. A count is counted where the rows are
//! instead of every row crossing the boundary so something else can call `len`
//! on them, and a zip is one transaction rather than two reads that have to be
//! trusted to agree.
//!
//! The whole plan is answered inside one transaction, so the objects a scan
//! asked for come from the same view as the rows that named them. The ordinary
//! path fetches them in a second call, because it cannot know which objects it
//! wants until the first one answers.

use doclite_core::{ObjectMode, Plan, ReadValue, Row, StorageError};

use crate::connection::Open;
use crate::held::Held;

/// A row beside the object it names, which is what a scan asking for objects
/// answers with.
type Beside = Vec<(Row, Option<Vec<u8>>)>;

pub(crate) fn plan(open: &mut Open, wanted: &Plan) -> Result<ReadValue, StorageError> {
    Held::reading(open)?.answer(wanted)
}

impl Held<'_> {
    fn answer(&self, wanted: &Plan) -> Result<ReadValue, StorageError> {
        match wanted {
            Plan::Count { index, range } => Ok(ReadValue::Count(self.count(*index, range)?)),

            Plan::Object { id } => Ok(ReadValue::Object(self.object(*id)?)),

            Plan::Rows {
                index,
                range,
                limit,
                objects,
            } => {
                let found = self.rows(*index, range, *limit)?;
                match objects {
                    ObjectMode::None => Ok(ReadValue::Rows(found)),
                    ObjectMode::Strict | ObjectMode::Lenient => {
                        Ok(ReadValue::RowsWithObjects(self.beside(found)?))
                    }
                }
            }

            Plan::Zip(left, right) => Ok(ReadValue::Pair(
                Box::new(self.answer(left)?),
                Box::new(self.answer(right)?),
            )),
        }
    }

    /// Each row beside the object it names.
    ///
    /// A row whose object is missing keeps its empty slot rather than being
    /// dropped: whether that is an abort is the caller's rule, not this one's.
    fn beside(&self, found: Vec<Row>) -> Result<Beside, StorageError> {
        let ids: Vec<_> = found.iter().map(|row| row.object).collect();
        Ok(found.into_iter().zip(self.objects(&ids)?).collect())
    }
}
