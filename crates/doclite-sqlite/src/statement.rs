//! One statement and what fills it.
//!
//! SQL and its parameters travel together because they are one thing: a
//! `LIMIT ?` that arrives without the value it asked for is not a statement,
//! and nothing else would have caught that.
//!
//! The order of the values is the order the statement binds them, which is the
//! order the placeholders appear in the text. How many there are is checked
//! here, so the only thing a caller still has to get right is the order.

use doclite_core::StorageError;
use rusqlite::types::{ToSqlOutput, Value};

pub(crate) struct Statement {
    sql: String,
    values: Vec<ToSqlOutput<'static>>,
}

impl Statement {
    /// Refuses SQL whose placeholders and values disagree.
    ///
    /// Every caller writes the text in one loop and the values in another, so
    /// this is the mistake the shape invites. Without it a missing value
    /// reaches SQLite and comes back naming a parameter index, a long way from
    /// the loop that dropped it.
    pub(crate) fn new(
        sql: String,
        values: Vec<ToSqlOutput<'static>>,
    ) -> Result<Self, StorageError> {
        let slots = placeholders(&sql);
        if slots != values.len() {
            return Err(StorageError::Engine(format!(
                "a statement with {slots} placeholders and {} values: {sql}",
                values.len()
            )));
        }
        Ok(Self { sql, values })
    }

    pub(crate) fn sql(&self) -> &str {
        &self.sql
    }

    /// The text and its values, for a caller building a wider statement around
    /// this one. The only way to take either, so neither can be bound onto text
    /// it does not belong to.
    pub(crate) fn into_parts(self) -> (String, Vec<ToSqlOutput<'static>>) {
        (self.sql, self.values)
    }

    /// In the order the statement binds them.
    pub(crate) fn values(&self) -> &[ToSqlOutput<'static>] {
        &self.values
    }
}

/// How many values the text asks to bind.
///
/// A `?` between quotes belongs to an identifier rather than to the statement,
/// and an index name comes from a caller's source, so one can hold anything.
/// Doubled quotes need no case of their own: the pair toggles twice and leaves
/// what sits between them inside the identifier, which is where it is.
fn placeholders(sql: &str) -> usize {
    let mut count = 0;
    let mut quoted = false;
    for byte in sql.bytes() {
        match byte {
            b'"' => quoted = !quoted,
            b'?' if !quoted => count += 1,
            _ => {}
        }
    }
    count
}

/// A row count as SQLite holds one.
///
/// Saturating rather than casting, because a limit wider than SQLite can hold
/// is not a limit and wrapping it into a negative one would be.
pub(crate) fn rows(count: usize) -> ToSqlOutput<'static> {
    ToSqlOutput::Owned(Value::Integer(count.min(i64::MAX as usize) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value() -> ToSqlOutput<'static> {
        ToSqlOutput::Owned(Value::Integer(1))
    }

    #[test]
    fn a_statement_whose_values_fill_it_holds() {
        let held = Statement::new(
            "SELECT ? FROM t WHERE c0 = ?".into(),
            vec![value(), value()],
        )
        .expect("two placeholders take two values");
        assert_eq!(held.values().len(), 2);
    }

    /// The class the check exists for: a `LIMIT ?` written into the text by one
    /// branch and its value pushed by another.
    #[test]
    fn a_placeholder_nothing_fills_is_refused() {
        let Err(StorageError::Engine(detail)) =
            Statement::new("SELECT c0 FROM t LIMIT ?".into(), Vec::new())
        else {
            panic!("a placeholder without a value is not a statement");
        };
        assert!(detail.contains("1 placeholders"), "it counts: {detail}");
        assert!(detail.contains("0 values"), "both sides: {detail}");
        assert!(detail.contains("LIMIT ?"), "and shows the SQL: {detail}");
    }

    #[test]
    fn a_value_no_placeholder_asks_for_is_refused() {
        let refused = Statement::new("DELETE FROM t".into(), vec![value()]);
        assert!(refused.is_err(), "a value nothing binds is a lost value");
    }

    /// Index names arrive from a caller's source, so a table can be called
    /// anything a Rust string can hold.
    #[test]
    fn a_question_mark_inside_an_identifier_binds_nothing() {
        Statement::new("DELETE FROM \"idx_a?b\"".into(), Vec::new())
            .expect("a name is not a placeholder");
        Statement::new(
            "DELETE FROM \"idx_a\"\"?\" WHERE c0 = ?".into(),
            vec![value()],
        )
        .expect("a doubled quote does not end the name");
    }
}
