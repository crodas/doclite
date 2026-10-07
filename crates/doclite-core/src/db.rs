//! Where BEGIN and COMMIT went.

use std::collections::BTreeMap;

use async_trait::async_trait;

use crate::check::Check;
use crate::engine::{Defined, Engine};
use crate::executor::answer::answer;
use crate::executor::walk::{walk, Attempt};
use crate::model::index::{ColumnType, IndexId, IndexSchema};
use crate::operation::Operation;
use crate::outcome::error::StorageError;
use crate::outcome::Outcome;
use crate::query::value::FromReadValue;
use crate::query::Query;

/// A store, the indexes it was told about, the retry loop, and which of those
/// indexes the engine said it has rows missing from.
///
/// This is where BEGIN and COMMIT went. There is no transaction handle to hold
/// because [`Db::run`] is the boundary, and no rollback to call because an
/// operation that does not commit has staged nothing.
///
/// What is building is reported and not acted on. Filling an index means
/// deriving rows from objects, and an object is bytes to everything at this
/// level, so a handle here could hold a read back forever with nothing able to
/// release it. [`Db::building`] is the report; waiting on it belongs to the
/// layer that can do something about it.
pub struct Db<E> {
    engine: E,
    schemas: Vec<IndexSchema>,
    attempts: usize,
    building: Defined,
}

/// The catalog, with an index named once.
///
/// Declaring the same index twice is ordinary: two records can be opened
/// together and a helper concatenates what each of them asks for. Declaring it
/// two different ways is not, and it has to be refused here, because nothing
/// downstream can tell the two apart. An engine takes the last one it is given
/// and a row filed against the other then fails as an arity the caller never
/// wrote, a long way from the catalog that caused it.
///
/// What has been declared is looked up by name rather than scanned for, so a
/// catalog costs its own size to build rather than its size squared. The order
/// the caller declared in is kept, because it is the order an engine is then
/// handed and the order a refusal reads in.
fn declared(indexes: &[IndexSchema]) -> Result<Vec<IndexSchema>, StorageError> {
    let mut held: BTreeMap<IndexId, &'static [ColumnType]> = BTreeMap::new();
    let mut named: Vec<IndexSchema> = Vec::with_capacity(indexes.len());
    for schema in indexes {
        match held.insert(schema.id, schema.columns) {
            None => named.push(*schema),
            Some(first) if first == schema.columns => {}
            Some(first) => {
                return Err(StorageError::Redeclared {
                    index: schema.id,
                    first,
                    second: schema.columns,
                })
            }
        }
    }
    Ok(named)
}

impl<E: Engine> Db<E> {
    /// Declares every index up front, so an operation's handles resolve
    /// against a catalog this handle already holds.
    pub async fn open(engine: E, indexes: &[IndexSchema]) -> Result<Self, StorageError> {
        let schemas = declared(indexes)?;
        let building = engine.define(&schemas).await?;
        Ok(Self {
            engine,
            schemas,
            attempts: 16,
            building,
        })
    }

    /// How many times a losing operation is re-walked before giving up.
    pub fn with_attempts(mut self, attempts: usize) -> Self {
        self.attempts = attempts;
        self
    }

    /// The engine underneath, for the engine-specific things `Db` puts no seam over.
    pub fn engine(&self) -> &E {
        &self.engine
    }

    /// The catalog as `open` settled it: deduplicated, in the order it was declared.
    pub fn schemas(&self) -> &[IndexSchema] {
        &self.schemas
    }

    /// Which indexes the engine said it has rows missing from.
    ///
    /// Empty for an engine that keeps nothing across a restart, because a store
    /// built from nothing cannot be behind. A caller that can derive rows from
    /// objects fills each one and says so with [`Db::rebuilt`]; a caller at this
    /// level that cannot should read this rather than query an index whose rows
    /// are not all there.
    pub fn building(&self) -> &[IndexId] {
        self.building.indexes()
    }

    /// Records that an index now holds every row it should, so the next open
    /// does not fill it again.
    pub async fn rebuilt(&self, index: IndexId) -> Result<(), StorageError> {
        self.engine.rebuilt(index).await
    }

    /// Runs one operation or one query, as one transaction.
    ///
    /// Work is taken by reference because losing to a concurrent writer costs
    /// nothing but a second walk: an operation is a value with no side effects,
    /// so `run` re-runs it rather than handing the conflict back.
    pub async fn run<R: Runnable<E>>(&self, work: &R) -> Result<Outcome<R::Output>, StorageError> {
        work.execute(self).await
    }
}

/// What `run` accepts.
#[async_trait]
pub trait Runnable<E: Engine>: Sync {
    /// What a committed run answers with.
    type Output;

    /// Takes the handle rather than an engine, because the catalog a check needs
    /// and the retry loop a conflict needs both live on the handle.
    async fn execute(&self, db: &Db<E>) -> Result<Outcome<Self::Output>, StorageError>;
}

#[async_trait]
impl<E: Engine, T: Clone + Send + Sync> Runnable<E> for Operation<T> {
    type Output = T;

    async fn execute(&self, db: &Db<E>) -> Result<Outcome<T>, StorageError> {
        self.check(db.schemas())?;
        for _ in 0..db.attempts {
            match walk(db, self.steps()).await? {
                Attempt::Committed => return Ok(Outcome::Committed(self.value())),
                Attempt::Aborted(code) => return Ok(Outcome::Aborted(code)),
                Attempt::Conflict => continue,
            }
        }
        Err(StorageError::Conflict {
            attempts: db.attempts,
        })
    }
}

#[async_trait]
impl<E: Engine, T: FromReadValue + Send + Sync> Runnable<E> for Query<T> {
    type Output = T;

    async fn execute(&self, db: &Db<E>) -> Result<Outcome<T>, StorageError> {
        Check::against(db.schemas()).plan(self.plan())?;
        match answer(db, self.plan()).await? {
            Outcome::Committed(value) => Ok(Outcome::Committed(T::from_read_value(value)?)),
            Outcome::Aborted(code) => Ok(Outcome::Aborted(code)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::index::Index;

    /// A helper that concatenates what two records ask for hands the same index
    /// over twice, so the second one is dropped rather than declared again.
    #[test]
    fn an_index_declared_twice_the_same_way_is_declared_once() {
        let users: Index<1> = Index::new("users_by_name");
        let notes: Index<1> = Index::new("notes_by_slug");
        let catalog = declared(&[users.schema(), notes.schema(), users.schema()])
            .expect("the same declaration twice is one declaration");

        let names: Vec<IndexId> = catalog.iter().map(|schema| schema.id).collect();
        assert_eq!(
            names,
            vec![IndexId("users_by_name"), IndexId("notes_by_slug")]
        );
    }

    /// Which of the two is `first` is the whole use of the message, so it is
    /// the declaration that got there first rather than the one that lost.
    #[test]
    fn a_disagreement_names_the_declaration_that_was_there_first() {
        let one: Index<1> = Index::typed("moving", &[ColumnType::Str]);
        let two: Index<2> = Index::typed("moving", &[ColumnType::Str, ColumnType::U32]);

        let Err(StorageError::Redeclared {
            index,
            first,
            second,
        }) = declared(&[one.schema(), two.schema()])
        else {
            panic!("an index cannot be declared two ways");
        };
        assert_eq!(index, IndexId("moving"));
        assert_eq!(first, &[ColumnType::Str]);
        assert_eq!(second, &[ColumnType::Str, ColumnType::U32]);
    }
}
