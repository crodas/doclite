//! A whole `for_each` as SQL statements.
//!
//! A template is a branchless construction, one per row, which is what
//! `INSERT INTO ... SELECT` already is. Compiling one either succeeds and the
//! engine takes the sweep, or fails and the executor expands it row by row, so
//! every refusal here is a fallback rather than an error the caller sees.
//!
//! The `ORDER BY` and `LIMIT` are not decoration. The executor takes the first
//! rows of the scan in index order, and a statement that took a different set
//! would be a different answer rather than the same one arrived at faster.

use doclite_core::{
    Bound, Column, IndexId, KeyRange, ObjectRef, RangeSpec, Step, StorageError, Sweep,
};
use rusqlite::types::ToSqlOutput;

use crate::catalog::{self, Catalog, Layout, OBJECT};
use crate::connection::Open;
use crate::held::Writing;
use crate::statement::Statement;
use crate::storage::{ObjectKey, Storage};

/// A whole sweep as statements.
///
/// Building one is how the engine decides whether to take the sweep: there is
/// no shorter way to know, and one it cannot express has to be expanded by the
/// executor instead.
pub(crate) struct Compiled(Vec<Statement>);

impl Compiled {
    pub(crate) fn of(catalog: &Catalog, sweep: &Sweep) -> Result<Self, StorageError> {
        Compiler::over(catalog, sweep)?.statements().map(Self)
    }
}

/// Whether this engine can write the whole sweep.
///
/// What compiled is kept, because the executor asks this and then applies the
/// same sweep, so the answer is the work.
pub(crate) fn takes(open: &mut Open, sweep: &Sweep) -> bool {
    match Compiled::of(open.catalog(), sweep) {
        Ok(compiled) => {
            open.remember(Some((sweep.clone(), compiled)));
            true
        }
        Err(_declined) => {
            open.remember(None);
            false
        }
    }
}

impl Writing<'_> {
    /// Runs a sweep inside the transaction that commits it, which is what
    /// leaves its scan and the writes derived from it one atomic act.
    pub(crate) fn sweep(
        &self,
        sweep: &Sweep,
        compiled: Option<&(Sweep, Compiled)>,
    ) -> Result<(), StorageError> {
        let fresh;
        let statements = match compiled {
            Some((was, ready)) if was == sweep => ready,
            _ => {
                fresh = Compiled::of(self.catalog(), sweep)?;
                &fresh
            }
        };
        for statement in &statements.0 {
            self.execute(statement)?;
        }
        Ok(())
    }
}

/// One sweep being turned into statements.
///
/// It holds the three things every step of the walk needs, so no step has to be
/// handed them again.
struct Compiler<'a> {
    catalog: &'a Catalog,
    sweep: &'a Sweep,
    source: &'a Layout,
}

impl<'a> Compiler<'a> {
    fn over(catalog: &'a Catalog, sweep: &'a Sweep) -> Result<Self, StorageError> {
        Ok(Self {
            catalog,
            sweep,
            source: catalog.get(sweep.index)?,
        })
    }

    /// The statements this sweep is, or why it is not any.
    ///
    /// A template that mixes filing and retiring is refused rather than
    /// ordered. Expanding one stages every retirement before every filing and
    /// drops a filing a later retirement covers, and reproducing that in
    /// statement order is a rule nobody has needed yet.
    fn statements(&self) -> Result<Vec<Statement>, StorageError> {
        let template = &self.sweep.template;
        let files = template
            .iter()
            .filter(|step| matches!(step, Step::PutIndex { .. }))
            .count();
        let retires = template
            .iter()
            .filter(|step| matches!(step, Step::DeleteIndex { .. }))
            .count();

        if template.len() != files + retires {
            return Err(refused("a template step that is not a row of an index"));
        }
        if files > 0 && retires > 0 {
            return Err(refused("a template that both files and retires"));
        }

        template
            .iter()
            .map(|step| match step {
                Step::PutIndex {
                    index,
                    columns,
                    object,
                } => self.file(*index, columns, object),
                Step::DeleteIndex { index, range } => self.retire(*index, range),
                _ => Err(refused("a template step that is not a row of an index")),
            })
            .collect()
    }

    /// `INSERT OR IGNORE INTO target (..) SELECT .. FROM source WHERE ..`.
    fn file(
        &self,
        index: IndexId,
        columns: &[Column],
        object: &ObjectRef,
    ) -> Result<Statement, StorageError> {
        let target = self.catalog.get(index)?;
        if columns.len() != target.arity() {
            return Err(refused("a filed row of the wrong width"));
        }

        let mut values = Vec::new();
        let mut exprs = Vec::with_capacity(columns.len() + 1);
        for (column, held) in columns.iter().zip(target.columns()) {
            exprs.push(self.copied(*held, column, &mut values)?);
        }
        exprs.push(match object {
            ObjectRef::Row => OBJECT.to_string(),
            ObjectRef::Id(id) => {
                values.push(ToSqlOutput::from(ObjectKey::of(*id).value()));
                "?".to_string()
            }
        });

        let (clause, selected) = self.source.predicate(&self.sweep.range)?.into_parts();
        values.extend(selected);
        Statement::new(
            format!(
                "INSERT OR IGNORE INTO {} ({}, {OBJECT}) SELECT {} FROM {}{} {}",
                target.table(),
                target.names(),
                exprs.join(", "),
                self.source.table(),
                clause,
                self.ordered(),
            ),
            values,
        )
    }

    /// `DELETE FROM target WHERE (..) IN (SELECT .. FROM source WHERE ..)`.
    ///
    /// A range naming no row is one delete rather than one per row, because
    /// running the same delete again changes nothing. That holds for every
    /// count but zero, where the executor would have run it none, so the delete
    /// is witnessed by the scan rather than issued beside it.
    fn retire(&self, index: IndexId, range: &RangeSpec) -> Result<Statement, StorageError> {
        let target = self.catalog.get(index)?;

        if let Some(fixed) = literal(range) {
            return target.delete_witnessed(target.predicate(&fixed)?, self.scanned()?);
        }

        let RangeSpec::Exact(columns) = range else {
            return Err(refused("a retired range that names a row and is not exact"));
        };
        if columns.len() != target.arity() {
            return Err(refused("a retired range of the wrong width"));
        }

        let mut values = Vec::new();
        let mut exprs = Vec::with_capacity(columns.len());
        for (column, held) in columns.iter().zip(target.columns()) {
            exprs.push(self.copied(*held, column, &mut values)?);
        }

        let (clause, selected) = self.source.predicate(&self.sweep.range)?.into_parts();
        values.extend(selected);
        Statement::new(
            format!(
                "DELETE FROM {} WHERE ({}) IN (SELECT {} FROM {}{} {})",
                target.table(),
                target.names(),
                exprs.join(", "),
                self.source.table(),
                clause,
                self.ordered(),
            ),
            values,
        )
    }

    /// What one target column reads as, and what it costs to say so.
    ///
    /// A column copied from the source has to be held the same way at both
    /// ends. A text column copied into a blob one would put two storage classes
    /// in one column, and SQLite orders text before blobs, so the target index
    /// would come back in the wrong order and nothing would say why.
    fn copied(
        &self,
        target: Storage,
        column: &Column,
        values: &mut Vec<ToSqlOutput<'static>>,
    ) -> Result<String, StorageError> {
        match column {
            Column::Literal(bytes) => {
                values.push(target.encode(bytes)?);
                Ok("?".to_string())
            }
            Column::RowColumn(at) => {
                let held = self
                    .source
                    .columns()
                    .get(*at)
                    .ok_or_else(|| refused("a column the scanned index does not have"))?;
                if *held != target {
                    return Err(refused("a column held one way here and another there"));
                }
                Ok(catalog::column(*at))
            }
            Column::RowObject => {
                // The object column is an integer, so copying it lands the
                // right value only in a column that is also one. Into anything
                // else the executor's encoding would have to be reproduced in
                // SQL, and declining costs a row-by-row expansion rather than
                // correctness.
                if !matches!(target, Storage::Integer { .. }) {
                    return Err(refused(
                        "an object written into a column that is not an integer",
                    ));
                }
                Ok(OBJECT.to_string())
            }
        }
    }

    /// The sweep's own scan, as a subquery that finds a row when the scan does.
    fn scanned(&self) -> Result<Statement, StorageError> {
        let (clause, values) = self.source.predicate(&self.sweep.range)?.into_parts();
        Statement::new(
            format!(
                "SELECT 1 FROM {}{} {}",
                self.source.table(),
                clause,
                self.ordered()
            ),
            values,
        )
    }

    /// The order and the cut the executor would have taken.
    fn ordered(&self) -> String {
        let by = self.source.names();
        match self.sweep.limit {
            Some(rows) => format!("ORDER BY {by}, {OBJECT} LIMIT {rows}"),
            None => format!("ORDER BY {by}, {OBJECT}"),
        }
    }
}

/// The range as fixed bytes, if it names no row at all.
fn literal(range: &RangeSpec) -> Option<KeyRange> {
    let fixed = |columns: &Vec<Column>| {
        columns
            .iter()
            .map(|column| match column {
                Column::Literal(bytes) => Some(bytes.clone()),
                Column::RowColumn(_) | Column::RowObject => None,
            })
            .collect::<Option<Vec<_>>>()
    };
    let side = |end: &Option<(Vec<Column>, Bound)>| match end {
        None => Some(None),
        Some((columns, bound)) => fixed(columns).map(|key| Some((key, *bound))),
    };
    match range {
        RangeSpec::All => Some(KeyRange::All),
        RangeSpec::Exact(columns) => fixed(columns).map(KeyRange::Exact),
        RangeSpec::Prefix(columns) => fixed(columns).map(KeyRange::Prefix),
        RangeSpec::Between { lo, hi } => Some(KeyRange::Between {
            lo: side(lo)?,
            hi: side(hi)?,
        }),
    }
}

/// A sweep this engine will not write, which is a fallback and never an error
/// the caller sees.
fn refused(why: &str) -> StorageError {
    StorageError::Engine(format!("a sweep that is not one statement: {why}"))
}
