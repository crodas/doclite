//! Answering scans and objects from one view.
//!
//! One transaction spans the whole call, which is what `Query::zip` rests on
//! and the only reason several reads can be trusted to agree.

use doclite_core::{Answers, IndexId, KeyRange, ObjectId, StorageError};

use crate::connection::Open;
use crate::held::Held;

/// The scans and objects as the engine thread receives them.
///
/// `Reads` borrows and the work crosses a thread, so what arrives is owned. The
/// shape is otherwise the same and the answers are still positional.
pub(crate) fn read(
    open: &mut Open,
    scans: &[(IndexId, KeyRange, Option<usize>)],
    ids: &[ObjectId],
) -> Result<Answers, StorageError> {
    let held = Held::reading(open)?;

    let mut answered = Vec::with_capacity(scans.len());
    for (index, range, limit) in scans {
        answered.push(held.rows(*index, range, *limit)?);
    }

    Ok(Answers {
        scans: answered,
        objects: held.objects(ids)?,
    })
}
