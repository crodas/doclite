//! The rules an engine owes, one function each.
//!
//! Every case here talks to an [`Engine`] directly and checks something that is
//! not a hint. A case builds its own engine and defines the suite's indexes on
//! it, so nothing carries over from the case before.

use doclite_core::{
    Applied, Assertion, Batch, Engine, IndexEntry, IndexId, IndexRead, KeyRange, ObjectId,
    Observed, RangeDelete, Reads, Row, StorageError,
};

use crate::catalog::{schemas, BY_EMAIL, BY_PAIR};
use crate::failure::{expect, fails, Failure};
use crate::fresh::Fresh;

/// A fresh engine with every index the suite names already declared.
///
/// Every rule in the suite is built this way, which is what lets one list name
/// all of them rather than one list per shape.
async fn open<F: Fresh>() -> Result<F::Engine, Failure> {
    let engine = F::fresh().await?;
    let building = engine.define(&schemas()).await?;
    expect(
        building.is_ready(),
        format!(
            "a fresh engine owes no index any rows, and this one named {:?}",
            building.indexes()
        ),
    )?;
    Ok(engine)
}

/// A fresh engine says every index it was just told about is complete.
///
/// What `define` answers is not a hint: an engine that calls an index complete
/// while its rows are missing has not saved work, it has made the next query
/// wrong. A store built from nothing cannot be behind what was declared, so the
/// only honest answer here is that nothing is outstanding. An engine that keeps
/// nothing across a restart answers this way always.
pub async fn a_fresh_engine_owes_no_rows<F: Fresh>() -> Result<(), Failure> {
    let engine = F::fresh().await?;
    let building = engine.define(&schemas()).await?;

    expect(
        building.is_ready(),
        format!(
            "a store with nothing in it owes nothing, and this one named {:?}",
            building.indexes()
        ),
    )?;
    expect(
        !building.is_building(BY_EMAIL.id()),
        "an index just declared on an empty store holds every row it should",
    )
}

/// Saying an index is filled is accepted, and saying it twice is not an error.
///
/// Defaulted to doing nothing for an engine that keeps no state, which is why
/// this asserts only that neither call is refused.
pub async fn an_index_reported_rebuilt_is_accepted<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;
    engine.rebuilt(BY_EMAIL.id()).await?;
    engine.rebuilt(BY_EMAIL.id()).await?;
    Ok(())
}

/// An index reported filled is not named again, which is what makes a rebuild
/// happen once rather than on every open.
///
/// The first declaration is one index on a store with nothing in it, and the
/// object filed after it is what gives the second declaration some history to
/// be behind. Whatever that second call names is reported filled, so the third
/// must name nothing. An engine that keeps nothing across a restart names
/// nothing at either call and passes without being asked anything, which is
/// the truthful outcome rather than a skipped one; one that keeps its rows and
/// loses what it was told rebuilds everything on every open while answering
/// each query correctly, so this is the only place it is visible.
pub async fn an_index_reported_rebuilt_is_not_named_again<F: Fresh>() -> Result<(), Failure> {
    let engine = F::fresh().await?;
    let empty = engine.define(&[BY_EMAIL.schema()]).await?;
    expect(
        empty.is_ready(),
        format!(
            "a store with nothing in it owes nothing, and this one named {:?}",
            empty.indexes()
        ),
    )?;
    commit(&engine, &[], &[(ObjectId(1), b"one".to_vec())], &[], &[]).await?;

    let building = engine.define(&schemas()).await?;
    for index in building.indexes() {
        engine.rebuilt(*index).await?;
    }

    let again = engine.define(&schemas()).await?;
    expect(
        again.is_ready(),
        format!(
            "every index it named was reported filled, and it still names {:?}",
            again.indexes()
        ),
    )
}

/// A scan answers in the index's own order: columns, then object id.
///
/// The one read contract that is not a hint. An engine that returns a superset
/// costs time; an engine that returns the right rows in the wrong order gives
/// the wrong answer, and nothing above it can tell. Two rows share a key here,
/// so the "then object id" half is checked rather than assumed.
pub async fn a_scan_answers_in_index_order<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let insert = [
        entry(BY_PAIR.id(), &["b", "1"], 3),
        entry(BY_PAIR.id(), &["a", "2"], 1),
        entry(BY_PAIR.id(), &["a", "1"], 5),
        entry(BY_PAIR.id(), &["a", "1"], 2),
    ];
    commit(engine, &[], &[], &[], &insert).await?;

    let rows = scan(engine, BY_PAIR.id()).await?;
    let seen: Vec<(Vec<Vec<u8>>, ObjectId)> = rows
        .into_iter()
        .map(|row| (row.key.components().to_vec(), row.object))
        .collect();
    let wanted = vec![
        (vec![column("a"), column("1")], ObjectId(2)),
        (vec![column("a"), column("1")], ObjectId(5)),
        (vec![column("a"), column("2")], ObjectId(1)),
        (vec![column("b"), column("1")], ObjectId(3)),
    ];
    expect(seen == wanted, format!("got {seen:?}"))
}

/// Everything in a batch lands, or the batch is not applied.
pub async fn a_batch_applies_whole<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let objects = [(ObjectId(1), b"one".to_vec())];
    let insert = [entry(BY_EMAIL.id(), &["a@b.c"], 1)];
    commit(engine, &[], &objects, &[], &insert).await?;

    let rows = scan(engine, BY_EMAIL.id()).await?;
    expect(rows.len() == 1, format!("{} index rows", rows.len()))?;
    let found = fetch(engine, ObjectId(1)).await?;
    expect(found.is_some(), "the object is missing")
}

/// A batch whose assertion fails changes nothing at all.
pub async fn a_refused_batch_changes_nothing<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let seed = [entry(BY_EMAIL.id(), &["taken"], 1)];
    commit(engine, &[], &[], &[], &seed).await?;

    let asserts = [Assertion::Index {
        index: BY_EMAIL.id(),
        range: KeyRange::Exact(vec![column("taken")]),
        limit: None,
        observed: Observed::Absent,
    }];
    let objects = [(ObjectId(2), b"two".to_vec())];
    let delete = [RangeDelete {
        index: BY_EMAIL.id(),
        range: KeyRange::All,
    }];
    let insert = [entry(BY_EMAIL.id(), &["fresh"], 2)];
    let batch = Batch::Writes {
        asserts: &asserts,
        objects: &objects,
        delete: &delete,
        insert: &insert,
    };

    let applied = engine.apply(&batch).await?;
    expect(applied == Applied::Conflict, format!("{applied:?}"))?;

    let rows = scan(engine, BY_EMAIL.id()).await?;
    expect(rows.len() == 1, "the delete ran anyway")?;
    let stowaway = fetch(engine, ObjectId(2)).await?;
    expect(stowaway.is_none(), "the object landed anyway")
}

/// An assertion is judged against the state before the batch.
///
/// That is what lets one condition serve both "nothing may hold this key" and
/// "this key, which this writer is itself about to release".
pub async fn assertions_see_pre_batch_state<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let seed = [entry(BY_EMAIL.id(), &["mine"], 1)];
    commit(engine, &[], &[], &[], &seed).await?;

    let asserts = [Assertion::Index {
        index: BY_EMAIL.id(),
        range: KeyRange::Exact(vec![column("mine")]),
        limit: None,
        observed: Observed::Rows(vec![Row::new(vec![column("mine")], ObjectId(1))]),
    }];
    let delete = [RangeDelete {
        index: BY_EMAIL.id(),
        range: KeyRange::Exact(vec![column("mine")]),
    }];
    let insert = [entry(BY_EMAIL.id(), &["mine"], 2)];
    let batch = Batch::Writes {
        asserts: &asserts,
        objects: &[],
        delete: &delete,
        insert: &insert,
    };

    let applied = engine.apply(&batch).await?;
    expect(applied == Applied::Committed, format!("{applied:?}"))?;

    let rows = scan(engine, BY_EMAIL.id()).await?;
    expect(
        rows.len() == 1 && rows[0].object == ObjectId(2),
        format!("got {rows:?}"),
    )
}

/// An assertion that no longer holds is a conflict, not an error.
pub async fn a_stale_assertion_is_a_conflict<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let seed = [entry(BY_EMAIL.id(), &["a@b.c"], 1)];
    commit(engine, &[], &[], &[], &seed).await?;

    let asserts = [Assertion::Index {
        index: BY_EMAIL.id(),
        range: KeyRange::Exact(vec![column("a@b.c")]),
        limit: None,
        observed: Observed::Count(0),
    }];
    let batch = Batch::Writes {
        asserts: &asserts,
        objects: &[],
        delete: &[],
        insert: &[],
    };

    match engine.apply(&batch).await {
        Ok(Applied::Conflict) => Ok(()),
        Ok(Applied::Committed) => Err(fails("the batch committed on a false assertion")),
        Err(error) => Err(fails(format!("answered with an error: {error}"))),
    }
}

/// Every read of one call answers from one view.
pub async fn reads_share_one_view<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let objects = [(ObjectId(1), b"one".to_vec())];
    let insert = [entry(BY_EMAIL.id(), &["a@b.c"], 1)];
    commit(engine, &[], &objects, &[], &insert).await?;

    let range = KeyRange::All;
    let scans = [IndexRead::new(BY_EMAIL.id(), &range)];
    let wanted = [ObjectId(1)];
    let answers = engine
        .read(&Reads {
            scans: &scans,
            objects: &wanted,
        })
        .await?;

    let names_a_row = answers.scans.first().is_some_and(|rows| rows.len() == 1);
    let names_a_object = answers.objects.first().is_some_and(Option::is_some);
    expect(names_a_row && names_a_object, format!("got {answers:?}"))
}

/// An object read answers one slot per id, in the order asked, whether or not
/// the object is there.
pub async fn objects_answer_positionally<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let objects = [(ObjectId(2), b"two".to_vec())];
    commit(engine, &[], &objects, &[], &[]).await?;

    let wanted = [ObjectId(1), ObjectId(2), ObjectId(3)];
    let answers = engine.read(&Reads::objects(&wanted)).await?;

    expect(
        answers.objects == vec![None, Some(b"two".to_vec()), None],
        format!("got {:?}", answers.objects),
    )
}

/// An object is written once and never rewritten.
pub async fn objects_are_additive<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let objects = [(ObjectId(1), b"one".to_vec())];
    commit(engine, &[], &objects, &[], &[]).await?;

    let again = [(ObjectId(1), b"two".to_vec())];
    let batch = Batch::Writes {
        asserts: &[],
        objects: &again,
        delete: &[],
        insert: &[],
    };
    match engine.apply(&batch).await {
        Err(StorageError::DuplicateObject(ObjectId(1))) => Ok(()),
        other => Err(fails(format!("got {other:?}"))),
    }
}

/// An index nobody declared is named, not guessed at.
pub async fn an_unknown_index_is_named<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let range = KeyRange::All;
    let missing = IndexId("testkit_nobody_declared_this");
    let scans = [IndexRead::new(missing, &range)];
    match engine.read(&Reads::scans(&scans)).await {
        Err(StorageError::UnknownIndex(named)) if named == missing => Ok(()),
        other => Err(fails(format!("got {other:?}"))),
    }
}

/// An assertion made under a limit is judged on what the reader looked at.
///
/// A `for_each` reads the first `n` rows of a range and never sees the rest, so
/// a row appearing past `n` is not something it could have depended on. An
/// engine that re-evaluates the whole range instead refuses a batch that should
/// have landed, and the operation retries until it gives up.
pub async fn a_limited_assertion_judges_what_was_read<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let seed = [
        entry(BY_EMAIL.id(), &["a@b.c"], 1),
        entry(BY_EMAIL.id(), &["b@c.d"], 2),
        entry(BY_EMAIL.id(), &["c@d.e"], 3),
    ];
    commit(engine, &[], &[], &[], &seed).await?;

    let looked_at = vec![
        Row::new(vec![column("a@b.c")], ObjectId(1)),
        Row::new(vec![column("b@c.d")], ObjectId(2)),
    ];
    let asserts = [Assertion::index(
        BY_EMAIL.id(),
        KeyRange::All,
        Some(2),
        Observed::Rows(looked_at),
    )];
    let insert = [entry(BY_PAIR.id(), &["derived", "1"], 1)];
    let batch = Batch::Writes {
        asserts: &asserts,
        objects: &[],
        delete: &[],
        insert: &insert,
    };

    match engine.apply(&batch).await {
        Ok(Applied::Committed) => Ok(()),
        Ok(Applied::Conflict) => Err(fails(
            "a third row outside the limit refused a batch that only read two",
        )),
        Err(error) => Err(fails(format!("answered with an error: {error}"))),
    }
}

/// Several scans in one call answer positionally, in the order asked.
///
/// One scan per call never shows this: an engine that answers two scans in the
/// wrong order passes every other case, and the executor pairs answers to
/// requests by position with nothing to catch a swap.
pub async fn scans_answer_positionally<F: Fresh>() -> Result<(), Failure> {
    let engine = &open::<F>().await?;

    let seed = [
        entry(BY_EMAIL.id(), &["a@b.c"], 1),
        entry(BY_PAIR.id(), &["clinic", "1"], 2),
        entry(BY_PAIR.id(), &["clinic", "2"], 3),
    ];
    commit(engine, &[], &[], &[], &seed).await?;

    let range = KeyRange::All;
    let scans = [
        IndexRead::new(BY_PAIR.id(), &range),
        IndexRead::new(BY_EMAIL.id(), &range),
    ];
    let answers = engine.read(&Reads::scans(&scans)).await?;

    let counts: Vec<usize> = answers.scans.iter().map(Vec::len).collect();
    expect(
        counts == vec![2, 1],
        format!("asked the two-row index first, got {counts:?}"),
    )
}

fn column(value: &str) -> Vec<u8> {
    value.as_bytes().to_vec()
}

fn entry(index: IndexId, columns: &[&str], id: u64) -> IndexEntry {
    IndexEntry {
        index,
        columns: columns.iter().copied().map(column).collect(),
        object: ObjectId(id),
    }
}

/// Sets a case up, and fails it if the setup itself lost a race.
async fn commit<E: Engine>(
    engine: &E,
    asserts: &[Assertion],
    objects: &[(ObjectId, Vec<u8>)],
    delete: &[RangeDelete],
    insert: &[IndexEntry],
) -> Result<(), Failure> {
    let batch = Batch::Writes {
        asserts,
        objects,
        delete,
        insert,
    };
    match engine.apply(&batch).await? {
        Applied::Committed => Ok(()),
        Applied::Conflict => Err(fails("setting the case up lost to a conflict")),
    }
}

async fn scan<E: Engine>(engine: &E, index: IndexId) -> Result<Vec<Row>, Failure> {
    let range = KeyRange::All;
    let scans = [IndexRead::new(index, &range)];
    let mut answers = engine.read(&Reads::scans(&scans)).await?;
    answers
        .scans
        .pop()
        .ok_or_else(|| fails("the engine answered no rows at all for a scan"))
}

async fn fetch<E: Engine>(engine: &E, id: ObjectId) -> Result<Option<Vec<u8>>, Failure> {
    let wanted = [id];
    let mut answers = engine.read(&Reads::objects(&wanted)).await?;
    answers
        .objects
        .pop()
        .ok_or_else(|| fails("the engine answered no slot at all for an object read"))
}
