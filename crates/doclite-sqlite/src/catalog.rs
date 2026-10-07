//! Which table an index lives in, what its columns hold, and what selects them.
//!
//! An engine keeps this itself because an `IndexId` is a `&'static str` and no
//! name read back out of the database can become one. The database is only ever
//! asked whether a table exists; what it is called comes from `define`.
//!
//! A `WHERE` clause lives here too. It was its own module, and the two imported
//! each other: a predicate is built from a layout's columns and consumed by a
//! layout's statement, so there was never a moment one existed without the
//! other. A partial bound is compared over the columns it names and no further,
//! which is what a row-value comparison does and why it is the construct here
//! rather than a convenience. Row values need SQLite 3.15.

use std::collections::BTreeMap;

use doclite_core::{Bound, IndexId, IndexSchema, KeyRange, StorageError};
use rusqlite::types::ToSqlOutput;

use crate::statement::{self, Statement};
use crate::storage::{ObjectKey, Storage};

/// What the column at this position is called in every table this engine owns.
///
/// One place, because a key column is named by the catalog that declares it,
/// by the predicate that compares it and by a sweep that copies it, and
/// renaming one was a three-file change with no type to hang it on.
pub(crate) fn column(at: usize) -> String {
    format!("c{at}")
}

/// The column holding the object a row names.
pub(crate) const OBJECT: &str = "object";

/// What every table this engine owns is prefixed with, so a database may hold
/// other tables without them being mistaken for indexes.
const INDEX_PREFIX: &str = "doclite_idx_";

/// Objects whose bytes are not text, which is every CBOR record and anything
/// the byte-level API files.
pub(crate) const DOCUMENTS: &str = "doclite_document";

/// Objects whose bytes are text, which is what `#[storage(json)]` writes.
///
/// A table of its own rather than a second storage class in one column, so each
/// `body` holds exactly one kind and `sqlite3` can be pointed at the legible
/// half without picking through the other.
pub(crate) const JSON_DOCUMENTS: &str = "doclite_json_document";

/// One index as this engine holds it.
pub(crate) struct Layout {
    id: IndexId,
    table: String,
    columns: Vec<Storage>,
}

impl Layout {
    /// What an index is called here follows from its name, so the prefix is
    /// applied once rather than by whoever builds one.
    pub(crate) fn of(id: IndexId, columns: Vec<Storage>) -> Self {
        Self {
            id,
            table: quoted(&format!("{INDEX_PREFIX}{}", id.0)),
            columns,
        }
    }

    pub(crate) fn table(&self) -> &str {
        &self.table
    }

    /// The same name unquoted, which is how `sqlite_master` holds it.
    pub(crate) fn name(&self) -> String {
        format!("{INDEX_PREFIX}{}", self.id.0)
    }

    /// What holds each column, in key order.
    pub(crate) fn columns(&self) -> &[Storage] {
        &self.columns
    }

    pub(crate) fn arity(&self) -> usize {
        self.columns.len()
    }

    /// `c0, c1, c2`.
    pub(crate) fn names(&self) -> String {
        (0..self.arity()).map(column).collect::<Vec<_>>().join(", ")
    }

    /// What selects exactly the rows of this range and nothing wider.
    pub(crate) fn predicate(&self, range: &KeyRange) -> Result<Predicate, StorageError> {
        Predicate::over(range, self)
    }

    /// No `UNIQUE` anywhere: an index carries no constraint, and whether at
    /// most one object may hold a key is asserted by an operation. Two rows may
    /// share a key, so the row itself is the primary key.
    pub(crate) fn create_table(&self) -> String {
        let declared = (0..self.arity())
            .map(|at| format!("{} {} NOT NULL", column(at), self.columns[at].declared()))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "CREATE TABLE IF NOT EXISTS {} ({declared}, {OBJECT} INTEGER NOT NULL, \
             PRIMARY KEY ({}, {OBJECT})) WITHOUT ROWID",
            self.table,
            self.names()
        )
    }

    /// The rows of a range, in index order: columns, then object.
    ///
    /// The order is stated rather than left to the table's own, because it is
    /// the one thing about a read that is not a hint. The limit is bound rather
    /// than written into the text, so every limit is one statement instead of
    /// one each.
    pub(crate) fn select(
        &self,
        predicate: Predicate,
        limit: Option<usize>,
    ) -> Result<Statement, StorageError> {
        let columns = self.names();
        let (clause, mut values) = predicate.into_parts();
        let cut = match limit {
            Some(count) => {
                values.push(statement::rows(count));
                " LIMIT ?"
            }
            None => "",
        };
        Statement::new(
            format!(
                "SELECT {columns}, {OBJECT} FROM {} {clause} ORDER BY {columns}, {OBJECT}{cut}",
                self.table
            ),
            values,
        )
    }

    /// How many rows a range holds, counted where they are.
    pub(crate) fn count(&self, predicate: Predicate) -> Result<Statement, StorageError> {
        let (clause, values) = predicate.into_parts();
        Statement::new(
            format!("SELECT COUNT(*) FROM {}{clause}", self.table),
            values,
        )
    }

    /// Every row of a range, whatever object it names.
    pub(crate) fn delete(&self, predicate: Predicate) -> Result<Statement, StorageError> {
        let (clause, values) = predicate.into_parts();
        Statement::new(format!("DELETE FROM {}{clause}", self.table), values)
    }

    /// The same delete, and only when `witness` finds a row.
    ///
    /// A swept `delete_index` over a fixed range is one delete rather than one
    /// per scanned row, because running the same delete again changes nothing.
    /// That argument covers every count but zero, and the witness is the scan
    /// itself, so no rows means no delete.
    ///
    /// The keyword is decided here because only this knows whether the range
    /// wrote a `WHERE` of its own. The range's values are bound before the
    /// witness's, which is the order the two appear in the text.
    pub(crate) fn delete_witnessed(
        &self,
        predicate: Predicate,
        witness: Statement,
    ) -> Result<Statement, StorageError> {
        let (clause, mut values) = predicate.into_parts();
        let keyword = if clause.is_empty() {
            " WHERE "
        } else {
            " AND "
        };
        let (found, witnessed) = witness.into_parts();
        values.extend(witnessed);
        Statement::new(
            format!(
                "DELETE FROM {}{clause}{keyword}EXISTS ({found})",
                self.table
            ),
            values,
        )
    }

    /// Filing a row that is already there is not an error: the reference keeps
    /// rows in a set, so a row filed twice is one row.
    ///
    /// The columns are encoded here because this is where what holds each one
    /// is known, which is one fewer place that has to agree with it. The width
    /// is checked here too, next to the indexing it guards, rather than only in
    /// the caller that happens to check it today.
    pub(crate) fn insert(
        &self,
        columns: &[Vec<u8>],
        object: ObjectKey,
    ) -> Result<Statement, StorageError> {
        if columns.len() != self.arity() {
            return Err(StorageError::Arity {
                index: self.id,
                expected: self.arity(),
                found: columns.len(),
            });
        }

        let mut values = Vec::with_capacity(self.arity() + 1);
        for (at, component) in columns.iter().enumerate() {
            values.push(self.columns[at].encode(component)?);
        }
        values.push(ToSqlOutput::from(object.value()));

        let slots = vec!["?"; self.arity() + 1].join(", ");
        Statement::new(
            format!(
                "INSERT OR IGNORE INTO {} ({}, {OBJECT}) VALUES ({slots})",
                self.table,
                self.names()
            ),
            values,
        )
    }
}

/// A predicate and the values it compares against, in the order they are bound.
pub(crate) struct Predicate {
    sql: String,
    values: Vec<ToSqlOutput<'static>>,
}

impl Predicate {
    /// The clause and what fills it, which is the only way to take either.
    ///
    /// A caller that could reach the values without the text it belongs to
    /// would be one that could bind them onto some other text.
    pub(crate) fn into_parts(self) -> (String, Vec<ToSqlOutput<'static>>) {
        (self.clause(), self.values)
    }

    /// The clause with its keyword, or nothing at all when the range takes
    /// everything.
    fn clause(&self) -> String {
        if self.sql.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.sql)
        }
    }

    /// What selects exactly the rows of this range, and nothing wider.
    ///
    /// Answering with more is legal on a read and is data loss on a delete, so
    /// this is written once and used by both rather than twice with a
    /// difference nobody notices.
    ///
    /// Private, because [`Layout::predicate`] is how one is built. Two public
    /// names for one operation is what this used to be.
    fn over(range: &KeyRange, layout: &Layout) -> Result<Predicate, StorageError> {
        let mut values = Vec::new();
        let sql = match range {
            KeyRange::All => String::new(),
            KeyRange::Exact(key) | KeyRange::Prefix(key) => {
                Self::equality(key, layout, &mut values)?
            }
            KeyRange::Between { lo, hi } => {
                let mut parts = Vec::new();
                if let Some((key, bound)) = lo {
                    let operator = if *bound == Bound::Incl { ">=" } else { ">" };
                    parts.push(Self::comparison(key, operator, layout, &mut values)?);
                }
                if let Some((key, bound)) = hi {
                    let operator = if *bound == Bound::Incl { "<=" } else { "<" };
                    parts.push(Self::comparison(key, operator, layout, &mut values)?);
                }
                parts.join(" AND ")
            }
        };
        Ok(Predicate { sql, values })
    }
}

impl Predicate {
    /// `c0 = ? AND c1 = ?`, over the columns the key names.
    ///
    /// `Exact` and `Prefix` differ in what the caller meant rather than in what
    /// selects it: an exact key names every column, and a prefix names the first
    /// few. Either way the named columns are equal and the rest are free.
    fn equality(
        key: &[Vec<u8>],
        layout: &Layout,
        values: &mut Vec<ToSqlOutput<'static>>,
    ) -> Result<String, StorageError> {
        if key.is_empty() {
            return Ok(String::new());
        }
        Self::bind(key, layout, values)?;
        Ok((0..key.len())
            .map(|at| format!("{} = ?", column(at)))
            .collect::<Vec<_>>()
            .join(" AND "))
    }

    /// `(c0, c1) >= (?, ?)`, over the columns the bound names.
    ///
    /// A single-column bound is written without the parentheses, because SQLite
    /// reads `(c0) >= (?)` as a scalar comparison anyway and the plain form is what
    /// a person reading the query expects.
    fn comparison(
        key: &[Vec<u8>],
        operator: &str,
        layout: &Layout,
        values: &mut Vec<ToSqlOutput<'static>>,
    ) -> Result<String, StorageError> {
        Self::bind(key, layout, values)?;
        if key.len() == 1 {
            return Ok(format!("{} {operator} ?", column(0)));
        }
        let columns = (0..key.len()).map(column).collect::<Vec<_>>().join(", ");
        let slots = vec!["?"; key.len()].join(", ");
        Ok(format!("({columns}) {operator} ({slots})"))
    }

    /// Encodes each component the way the column it names is stored.
    fn bind(
        key: &[Vec<u8>],
        layout: &Layout,
        values: &mut Vec<ToSqlOutput<'static>>,
    ) -> Result<(), StorageError> {
        if key.len() > layout.arity() {
            return Err(StorageError::Engine(format!(
                "a key of {} columns against an index of {}",
                key.len(),
                layout.arity()
            )));
        }
        for (at, component) in key.iter().enumerate() {
            values.push(layout.columns()[at].encode(component)?);
        }
        Ok(())
    }
}

/// Every index that has been defined, by the name it was defined under.
#[derive(Default)]
pub(crate) struct Catalog(BTreeMap<IndexId, Layout>);

impl Catalog {
    /// Declares an index, or refuses a second declaration that disagrees.
    ///
    /// `define` runs on every open, so declaring the same index again is
    /// ordinary. Declaring it differently within one call is not: there is no
    /// earlier state to reconcile and nothing says which of the two was meant.
    /// A shape that moved since the last run is a different question, and
    /// `version.rs` answers it by rebuilding the index rather than refusing.
    pub(crate) fn declare(&mut self, schema: &IndexSchema) -> Result<&Layout, StorageError> {
        let columns: Vec<Storage> = schema.columns.iter().map(Storage::of).collect();
        let held = self
            .0
            .entry(schema.id)
            .or_insert_with(|| Layout::of(schema.id, columns.clone()));
        if held.columns != columns {
            return Err(changed(schema.id, &held.columns, &columns));
        }
        Ok(held)
    }

    /// The index, named rather than guessed at when it is not there.
    pub(crate) fn get(&self, index: IndexId) -> Result<&Layout, StorageError> {
        self.0.get(&index).ok_or(StorageError::UnknownIndex(index))
    }
}

/// What moved between two declarations of one index, within one `define` call.
///
/// This is not about a shape that moved between two runs, which `version.rs`
/// settles by rebuilding the index. It is about one call declaring one index two
/// ways, where there is no earlier state to reconcile and no way to tell which
/// of the two the caller meant.
///
/// The width and a column's storage are two different mistakes, so they are two
/// different sentences. Reporting the width for both is how this said "two
/// columns and is now two" and told nobody anything.
fn changed(index: IndexId, held: &[Storage], now: &[Storage]) -> StorageError {
    let migration = "one call cannot declare one index two ways";

    if held.len() != now.len() {
        return StorageError::Engine(format!(
            "{index} was declared with {} columns and is now declared with {}; {migration}",
            held.len(),
            now.len()
        ));
    }

    match held.iter().zip(now).position(|(was, is)| was != is) {
        Some(at) => StorageError::Engine(format!(
            "{index} column {at} was declared as {} and is now declared as {}; {migration}",
            held[at].declared(),
            now[at].declared()
        )),
        None => StorageError::Engine(format!("{index} was declared twice over; {migration}")),
    }
}

/// A SQL identifier that cannot be read as anything else.
///
/// Index names arrive from a caller's source rather than from a user, but
/// quoting is what makes that a fact about this function instead of a hope
/// about every caller.
pub(crate) fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use doclite_core::{ColumnType, Index, ObjectId};

    use super::*;

    fn ranged(columns: &[ColumnType]) -> Layout {
        Layout::of(IndexId("t"), columns.iter().map(Storage::of).collect())
    }

    fn sql(range: &KeyRange, columns: &[ColumnType]) -> String {
        ranged(columns)
            .predicate(range)
            .expect("the range translates")
            .sql
    }

    #[test]
    fn everything_needs_no_predicate_at_all() {
        assert_eq!(sql(&KeyRange::All, &[ColumnType::Bytes]), "");
    }

    #[test]
    fn an_exact_key_compares_every_column() {
        let range = KeyRange::Exact(vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(sql(&range, &[ColumnType::Bytes; 2]), "c0 = ? AND c1 = ?");
    }

    #[test]
    fn a_prefix_compares_only_the_columns_it_names() {
        let range = KeyRange::Prefix(vec![b"a".to_vec()]);
        assert_eq!(sql(&range, &[ColumnType::Bytes; 2]), "c0 = ?");
    }

    #[test]
    fn a_bound_over_one_column_is_a_plain_comparison() {
        let range = KeyRange::Between {
            lo: Some((vec![b"a".to_vec()], Bound::Incl)),
            hi: None,
        };
        assert_eq!(sql(&range, &[ColumnType::Bytes; 2]), "c0 >= ?");
    }

    /// The whole reason for row values: a two-column bound on a three-column
    /// index selects whole groups rather than comparing column by column.
    #[test]
    fn a_partial_bound_is_a_row_value_comparison() {
        let range = KeyRange::Between {
            lo: Some((vec![b"a".to_vec(), b"b".to_vec()], Bound::Excl)),
            hi: None,
        };
        assert_eq!(sql(&range, &[ColumnType::Bytes; 3]), "(c0, c1) > (?, ?)");
    }

    #[test]
    fn the_two_sides_may_name_different_numbers_of_columns() {
        let range = KeyRange::Between {
            lo: Some((vec![b"a".to_vec()], Bound::Incl)),
            hi: Some((vec![b"b".to_vec(), b"c".to_vec()], Bound::Excl)),
        };
        assert_eq!(
            sql(&range, &[ColumnType::Bytes; 2]),
            "c0 >= ? AND (c0, c1) < (?, ?)"
        );
    }

    #[test]
    fn two_absent_endpoints_take_everything() {
        let range = KeyRange::Between { lo: None, hi: None };
        assert_eq!(sql(&range, &[ColumnType::Bytes]), "");
    }

    #[test]
    fn a_key_wider_than_the_index_is_refused() {
        let range = KeyRange::Exact(vec![b"a".to_vec(), b"b".to_vec()]);
        assert!(ranged(&[ColumnType::Bytes]).predicate(&range).is_err());
    }

    #[test]
    fn each_component_is_bound_in_the_order_it_is_compared() {
        let range = KeyRange::Exact(vec![b"a".to_vec(), b"b".to_vec()]);
        let built = ranged(&[ColumnType::Bytes; 2])
            .predicate(&range)
            .expect("translates");
        assert_eq!(built.values.len(), 2);
    }

    #[test]
    fn an_index_is_declared_once_and_found_by_its_name() {
        let index: Index<2> = Index::typed("users_by_pair", &[ColumnType::Str, ColumnType::U32]);
        let mut catalog = Catalog::default();
        catalog
            .declare(&index.schema())
            .expect("a first declaration holds");

        let found = catalog
            .get(IndexId("users_by_pair"))
            .expect("it was declared");
        assert_eq!(found.table(), "\"doclite_idx_users_by_pair\"");
        assert_eq!(found.arity(), 2);
    }

    #[test]
    fn an_index_nobody_declared_is_named_rather_than_guessed_at() {
        let catalog = Catalog::default();
        let Err(refused) = catalog.get(IndexId("nobody_declared_this")) else {
            panic!("an index nobody declared is not there");
        };
        assert_eq!(
            refused,
            StorageError::UnknownIndex(IndexId("nobody_declared_this"))
        );
    }

    /// `define` is called again on every open, so declaring twice is ordinary
    /// rather than an error.
    #[test]
    fn declaring_the_same_index_twice_is_the_same_table() {
        let index: Index<1> = Index::new("again");
        let mut catalog = Catalog::default();
        let first = catalog
            .declare(&index.schema())
            .expect("a first declaration holds")
            .table()
            .to_string();
        let second = catalog
            .declare(&index.schema())
            .expect("the same declaration again holds")
            .table()
            .to_string();
        assert_eq!(first, second);
    }

    /// The guard used to live only in `apply`, one file away from the indexing
    /// it protects: too many columns panicked here and too few bound fewer
    /// values than the statement had slots.
    #[test]
    fn a_row_of_the_wrong_width_is_refused_where_it_is_encoded() {
        let index: Index<2> = Index::typed("pairs", &[ColumnType::Str, ColumnType::U32]);
        let mut catalog = Catalog::default();
        let layout = catalog.declare(&index.schema()).expect("it declares");
        let object = ObjectKey::of(ObjectId(1));

        for columns in [vec![b"a".to_vec(); 3], vec![b"a".to_vec()]] {
            let found = columns.len();
            let Err(refused) = layout.insert(&columns, object) else {
                panic!("a row of {found} columns is not a row of this index");
            };
            assert_eq!(
                refused,
                StorageError::Arity {
                    index: IndexId("pairs"),
                    expected: 2,
                    found,
                }
            );
        }
    }

    #[test]
    fn a_name_carrying_a_quote_cannot_end_the_identifier() {
        assert_eq!(quoted("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn a_column_list_names_every_column_in_order() {
        let index: Index<3> = Index::new("three");
        let mut catalog = Catalog::default();
        let layout = catalog.declare(&index.schema()).expect("it declares");
        assert_eq!(layout.names(), "c0, c1, c2");
    }

    /// The width and a column's storage are two different mistakes, and this is
    /// the one that used to report the other's number and say "two columns and
    /// is now two".
    #[test]
    fn an_index_redeclared_with_another_storage_names_the_column() {
        let held: Index<2> = Index::typed("moving", &[ColumnType::U32, ColumnType::Str]);
        let now: Index<2> = Index::typed("moving", &[ColumnType::U64, ColumnType::Str]);
        let mut catalog = Catalog::default();
        catalog
            .declare(&held.schema())
            .expect("a first declaration holds");

        let Err(StorageError::Engine(detail)) = catalog.declare(&now.schema()) else {
            panic!("a column cannot quietly change what holds it");
        };
        assert!(detail.contains("column 0"), "it names the column: {detail}");
        assert!(detail.contains("INTEGER"), "and what it was: {detail}");
        assert!(detail.contains("BLOB"), "and what it is now: {detail}");
    }

    /// The table already exists and `CREATE TABLE IF NOT EXISTS` would leave it
    /// alone, so a second declaration that disagrees has to be refused here or
    /// it is invisible until a row is filed.
    #[test]
    fn an_index_redeclared_with_another_width_is_refused() {
        let one: Index<1> = Index::new("moving");
        let two: Index<2> = Index::new("moving");
        let mut catalog = Catalog::default();
        catalog
            .declare(&one.schema())
            .expect("a first declaration holds");

        let Err(refused) = catalog.declare(&two.schema()) else {
            panic!("an index cannot quietly change width");
        };
        let StorageError::Engine(detail) = refused else {
            panic!("only the engine can describe this");
        };
        assert!(detail.contains("moving"), "it names the index: {detail}");
    }
}
