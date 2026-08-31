//! What a declared column type costs and what it owes.
//!
//! The conformance suite declares every column `ColumnType::Bytes`, so none of
//! the mappings this engine makes are visible to it. Each one owes two things:
//! the rows come back in the order the encoded bytes sort, and the bytes come
//! back exactly as they went in. A typed column that got either wrong would
//! misorder an index or fail an assertion, and nothing else would notice.

use doclite_core::{
    all, ColumnType, Db, Index, ObjectId, Operation, Outcome, Query, Row, StorageError, ToOrdBytes,
};
use doclite_sqlite::SqliteEngine;

/// Files one row per value, reads them all back, and says what came out.
///
/// The object id counts up with the input, so it never decides the order: what
/// orders the answer is the column alone.
async fn filed(index: Index<1>, values: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, StorageError> {
    let db = Db::open(SqliteEngine::in_memory()?, &[index.schema()]).await?;

    let mut op = Operation::new();
    for (at, value) in values.iter().enumerate() {
        let id = ObjectId(at as u64 + 1);
        op = op.put_object(id, value.clone()).put_index(
            &index,
            doclite_core::key![value.clone()],
            id,
        );
    }
    let Outcome::Committed(()) = db.run(&op).await? else {
        panic!("filing a row is not something an assertion can refuse");
    };

    let Outcome::Committed(rows) = db.run(&Query::scan(&index, all::<1>())).await? else {
        panic!("a scan is not something an assertion can refuse");
    };
    Ok(rows
        .iter()
        .map(|row: &Row| row.key.components()[0].clone())
        .collect())
}

/// The values as the index should hand them back: sorted by their bytes, which
/// is what `ToOrdBytes` promises and what every mapping here has to preserve.
fn sorted(mut values: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    values.sort();
    values
}

async fn check(index: Index<1>, values: Vec<Vec<u8>>) -> Result<(), StorageError> {
    let found = filed(index, &values).await?;
    assert_eq!(found, sorted(values), "in order, and byte for byte");
    Ok(())
}

#[tokio::test]
async fn a_bool_column_orders_false_before_true() -> Result<(), StorageError> {
    check(
        Index::typed("c_bool", &[ColumnType::Bool]),
        vec![true.to_ord_bytes(), false.to_ord_bytes()],
    )
    .await
}

#[tokio::test]
async fn an_unsigned_column_orders_across_its_whole_width() -> Result<(), StorageError> {
    check(
        Index::typed("c_u8", &[ColumnType::U8]),
        vec![255u8, 0, 128, 1]
            .into_iter()
            .map(|n| n.to_ord_bytes())
            .collect(),
    )
    .await?;
    check(
        Index::typed("c_u32", &[ColumnType::U32]),
        vec![u32::MAX, 0, 1, u32::MAX / 2]
            .into_iter()
            .map(|n| n.to_ord_bytes())
            .collect(),
    )
    .await
}

/// The case a signed integer column exists to get right: two's complement puts
/// -1 above 1, and the encoding flips the sign bit so it does not.
#[tokio::test]
async fn a_signed_column_orders_across_zero() -> Result<(), StorageError> {
    check(
        Index::typed("c_i32", &[ColumnType::I32]),
        vec![0i32, -1, 1, i32::MIN, i32::MAX]
            .into_iter()
            .map(|n| n.to_ord_bytes())
            .collect(),
    )
    .await?;
    check(
        Index::typed("c_i64", &[ColumnType::I64]),
        vec![0i64, -1, i64::MIN, i64::MAX]
            .into_iter()
            .map(|n| n.to_ord_bytes())
            .collect(),
    )
    .await
}

/// Wider than SQLite's integers, so it stays bytes and still has to order.
#[tokio::test]
async fn a_column_too_wide_for_an_integer_still_orders() -> Result<(), StorageError> {
    check(
        Index::typed("c_u64", &[ColumnType::U64]),
        vec![u64::MAX, 0, 1, u64::MAX / 2]
            .into_iter()
            .map(|n| n.to_ord_bytes())
            .collect(),
    )
    .await?;
    check(
        Index::typed("c_i128", &[ColumnType::I128]),
        vec![0i128, -1, i128::MIN, i128::MAX]
            .into_iter()
            .map(|n| n.to_ord_bytes())
            .collect(),
    )
    .await
}

#[tokio::test]
async fn a_char_column_orders_by_code_point() -> Result<(), StorageError> {
    check(
        Index::typed("c_char", &[ColumnType::Char]),
        vec!['z', 'a', 'A', '\u{1F600}']
            .into_iter()
            .map(|c| c.to_ord_bytes())
            .collect(),
    )
    .await
}

/// The one column stored as text. An empty string and a prefix of another are
/// where a length-aware comparison would differ from a bytewise one.
#[tokio::test]
async fn a_text_column_orders_bytewise_and_keeps_what_it_held() -> Result<(), StorageError> {
    check(
        Index::typed("c_str", &[ColumnType::Str]),
        vec!["b", "", "a", "ab", "A", "ñ"]
            .into_iter()
            .map(|s| s.to_ord_bytes())
            .collect(),
    )
    .await
}

#[tokio::test]
async fn a_byte_column_keeps_bytes_that_are_not_text() -> Result<(), StorageError> {
    check(
        Index::typed("c_bytes", &[ColumnType::Bytes]),
        vec![vec![0xff, 0x00], Vec::new(), vec![0x00], vec![0x00, 0x00]],
    )
    .await
}

/// An absent value sorts before every present one, and the tag byte that makes
/// that true is why a nullable column is bytes rather than a null.
#[tokio::test]
async fn an_absent_value_sorts_before_every_present_one() -> Result<(), StorageError> {
    check(
        Index::typed("c_opt_u32", &[ColumnType::Nullable(&ColumnType::U32)]),
        vec![Some(7u32), None, Some(0), Some(u32::MAX)]
            .into_iter()
            .map(|v| v.to_ord_bytes())
            .collect(),
    )
    .await?;
    check(
        Index::typed("c_opt_str", &[ColumnType::Nullable(&ColumnType::Str)]),
        vec![
            Some("b".to_string()),
            None,
            Some(String::new()),
            Some("a".into()),
        ]
        .into_iter()
        .map(|v| v.to_ord_bytes())
        .collect(),
    )
    .await
}

/// A text column holds text, and what cannot be text is refused rather than
/// stored as something else.
///
/// A `Str` column is declared `TEXT`, and a blob written into it would put two
/// storage classes in one column. SQLite orders every text value below every
/// blob, so the index would come back in the wrong order and nothing would say
/// why. The byte-level API is what can reach this: a caller declaring `Str` and
/// filing bytes that are not.
#[tokio::test]
async fn a_text_column_refuses_bytes_that_are_not_text() -> Result<(), StorageError> {
    let index: Index<1> = Index::typed("c_not_text", &[ColumnType::Str]);
    let db = Db::open(SqliteEngine::in_memory()?, &[index.schema()]).await?;

    let id = ObjectId(1);
    let op = Operation::new()
        .put_object(id, b"a body".to_vec())
        .put_index(&index, doclite_core::key![vec![0xff_u8, 0xfe]], id);

    let Err(refused) = db.run(&op).await else {
        panic!("a text column cannot hold what is not text");
    };
    let StorageError::Engine(detail) = refused else {
        panic!("only the engine can describe this: {refused:?}");
    };
    assert!(detail.contains("utf-8"), "it says what was wrong: {detail}");
    Ok(())
}
