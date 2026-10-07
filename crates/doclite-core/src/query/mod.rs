//! Reads, as values that compose.
//!
//! [`Query`] is the caller's surface. It builds a [`plan::ReadPlan`], which is
//! plain data, and promises the shape [`value::ReadValue`] comes back in.

pub(crate) mod plan;
pub(crate) mod value;

use std::marker::PhantomData;

use crate::model::index::{Index, IndexId};
use crate::model::object::ObjectId;
use crate::model::range::Range;
use crate::model::row::Row;
use crate::model::storage::FromStorage;
use crate::query::plan::{ObjectMode, ReadPlan};

/// A read that knows what it will produce.
///
/// The type parameter is a promise about the answer's shape, not something the
/// plan carries. The plan stays plain data so it could be encoded and sent
/// somewhere that has no Rust types at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query<T> {
    plan: ReadPlan,
    output: PhantomData<fn() -> T>,
}

impl<T> Query<T> {
    fn wrap(plan: ReadPlan) -> Self {
        Self {
            plan,
            output: PhantomData,
        }
    }

    /// The plan as plain data, which is what an engine is handed and what a
    /// pushdown decides against.
    pub fn plan(&self) -> &ReadPlan {
        &self.plan
    }

    /// Every index this query reads, for a caller waiting on one whose rows are
    /// not all there yet.
    pub fn reads(&self) -> Vec<IndexId> {
        self.plan.reads()
    }
}

impl Query<u64> {
    /// How many rows the range holds.
    pub fn count<const N: usize, const K: usize>(index: &Index<N>, range: Range<N, K>) -> Self {
        Self::wrap(ReadPlan::Count {
            index: index.id(),
            range: range.into_spec(),
        })
    }
}

/// The one read whose output type is not fixed by its arguments, so it hangs off
/// the unit instantiation and takes the type as its own parameter.
impl Query<()> {
    /// One object, if it is there, decoded into `T`.
    ///
    /// `T` is whatever the caller asks for, so a store of bytes reads back as
    /// `Query::object::<Vec<u8>>` and a store of records as
    /// `Query::object::<User>`.
    pub fn object<T: FromStorage>(id: ObjectId) -> Query<Option<T>> {
        Query::wrap(ReadPlan::Object { id })
    }
}

impl Query<Vec<Row>> {
    /// Every row of the range, in the index's own order.
    pub fn scan<const N: usize, const K: usize>(index: &Index<N>, range: Range<N, K>) -> Self {
        Self::wrap(ReadPlan::Scan {
            index: index.id(),
            range: range.into_spec(),
            limit: None,
            objects: ObjectMode::None,
        })
    }

    /// The first `rows` of the range, taken after the range filter, so an
    /// engine that answers with more than was asked still yields these.
    pub fn limit(self, rows: usize) -> Self {
        Self::wrap(with_limit(self.plan, rows))
    }

    /// Also fetch each row's object, decoded into `T`. An index row naming a
    /// missing object aborts with
    /// [`DANGLING_INDEX_ENTRY`](crate::AbortCode::DANGLING_INDEX_ENTRY).
    ///
    /// Apply [`limit`](Query::limit) before this. The limit bounds the scan,
    /// and once a scan carries objects there is nothing left to bound.
    pub fn with_objects<T: FromStorage>(self) -> Query<Vec<(Row, T)>> {
        Query::wrap(with_objects(self.plan, ObjectMode::Strict))
    }

    /// Also fetch each row's object, yielding `None` for a missing one.
    pub fn with_objects_lenient<T: FromStorage>(self) -> Query<Vec<(Row, Option<T>)>> {
        Query::wrap(with_objects(self.plan, ObjectMode::Lenient))
    }
}

impl<A, B> Query<(A, B)> {
    /// Two reads, one view of the store.
    ///
    /// This is what replaces several reads in one program, and it nests, so
    /// four consistent reads stay one expression.
    pub fn zip(left: Query<A>, right: Query<B>) -> Self {
        Self::wrap(ReadPlan::Zip(Box::new(left.plan), Box::new(right.plan)))
    }
}

/// Only the scan-shaped queries expose `limit`, so the plan is always a scan
/// here; anything else is left alone rather than quietly reshaped.
fn with_limit(plan: ReadPlan, rows: usize) -> ReadPlan {
    match plan {
        ReadPlan::Scan {
            index,
            range,
            objects,
            limit: _,
        } => ReadPlan::Scan {
            index,
            range,
            objects,
            limit: Some(rows),
        },
        other => other,
    }
}

fn with_objects(plan: ReadPlan, mode: ObjectMode) -> ReadPlan {
    match plan {
        ReadPlan::Scan {
            index,
            range,
            limit,
            objects: _,
        } => ReadPlan::Scan {
            index,
            range,
            limit,
            objects: mode,
        },
        other => other,
    }
}
