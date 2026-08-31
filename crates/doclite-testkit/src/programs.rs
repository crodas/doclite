//! The worked programs, run against whichever engine is being verified.
//!
//! These do not test an engine directly the way [`cases`](crate::cases) does.
//! They drive the executor over one, which is the layer a backend is actually
//! used through, and they are here rather than in an engine crate because
//! every engine owes the same answers to them.
//!
//! They assert rather than returning a [`Failure`] for each check. A case
//! reports what an engine got wrong and a panic would bury that; a program is
//! checking the executor's arithmetic, where the message `assert_eq!` writes is
//! the more useful one.

use doclite_core::{
    all, exact, key, prefix, AbortCode, ColumnType, Db, Index, ObjectId, Operation, Outcome, Query,
    Row, StorageError,
};

use crate::failure::Failure;
use crate::fresh::Fresh;

static USERS_BY_EMAIL: Index<1> = Index::new("users_by_email");
static USERS_BY_CI: Index<1> = Index::new("users_by_ci");
static USERS_BY_COUNTRY: Index<2> = Index::new("users_by_country");
static APPTS: Index<3> = Index::new("appts_by_doctor_date_time");
static PATIENTS: Index<2> = Index::new("patients_by_clinic");
static PATIENTS_V2: Index<2> = Index::new("patients_by_clinic_v2");
static NOTES_BY_PATIENT: Index<1> = Index::new("notes_by_patient");

const DUPLICATE_EMAIL: AbortCode = AbortCode::new(100);
const DUPLICATE_CI: AbortCode = AbortCode::new(101);
const NO_SUCH_USER: AbortCode = AbortCode::new(102);
const INVARIANT: AbortCode = AbortCode::new(103);

/// A store holding nothing, with every index these programs name.
async fn open<F: Fresh>() -> Result<Db<F::Engine>, Failure> {
    let schemas = [
        USERS_BY_EMAIL.schema(),
        USERS_BY_CI.schema(),
        USERS_BY_COUNTRY.schema(),
        APPTS.schema(),
        PATIENTS.schema(),
        PATIENTS_V2.schema(),
        NOTES_BY_PATIENT.schema(),
    ];
    Ok(Db::open(F::fresh().await?, &schemas).await?)
}

/// The uniqueness rule, asserted by the operation rather than declared on the
/// index: the second run aborts with the caller's own code.
pub async fn insert_a_user_if_the_email_is_unique<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;
    let id = ObjectId(7);

    let op = Operation::new()
        .require_absent(&USERS_BY_EMAIL, exact(key!["a@b.c"]), DUPLICATE_EMAIL)
        .put_object(id, b"a blob".to_vec())
        .put_index(&USERS_BY_EMAIL, key!["a@b.c"], id)
        .returning(id);

    assert_eq!(db.run(&op).await?, Outcome::Committed(id));
    assert_eq!(db.run(&op).await?, Outcome::Aborted(DUPLICATE_EMAIL));
    Ok(())
}

/// Superseding is a delete of the old rows and an insert of the new ones, in
/// that order, rather than a step of its own.
pub async fn replace_a_logical_object<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;
    let old = ObjectId(1);
    let new = ObjectId(2);

    let create = Operation::new()
        .put_object(old, b"first".to_vec())
        .put_index(&USERS_BY_EMAIL, key!["old@b.c"], old);
    assert!(db.run(&create).await?.is_committed());

    let replace = Operation::new()
        .require_absent(&USERS_BY_EMAIL, exact(key!["new@b.c"]), DUPLICATE_EMAIL)
        .put_object(new, b"second".to_vec())
        .delete_index(&USERS_BY_EMAIL, exact(key!["old@b.c"]))
        .put_index(&USERS_BY_EMAIL, key!["new@b.c"], new)
        .returning(new);
    assert_eq!(db.run(&replace).await?, Outcome::Committed(new));

    let gone = Query::count(&USERS_BY_EMAIL, exact(key!["old@b.c"]));
    assert_eq!(db.run(&gone).await?, Outcome::Committed(0));

    let here = Query::count(&USERS_BY_EMAIL, exact(key!["new@b.c"]));
    assert_eq!(db.run(&here).await?, Outcome::Committed(1));
    Ok(())
}

/// A range scan is a range scan: the executor trims whatever an engine hands
/// back, so the answer does not depend on the engine honouring the hint.
pub async fn scan_a_range<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let mut fill = Operation::new();
    for (at, time) in ["09:00", "10:00", "11:00"].iter().enumerate() {
        let id = ObjectId(at as u64 + 1);
        fill = fill
            .put_object(id, format!("appointment at {time}").into_bytes())
            .put_index(&APPTS, key!["dr-a", "2026-08-26", *time], id);
    }
    fill = fill.put_index(&APPTS, key!["dr-a", "2026-08-27", "09:00"], ObjectId(9));
    assert!(db.run(&fill).await?.is_committed());

    let query = Query::scan(&APPTS, prefix(key!["dr-a", "2026-08-26"]))
        .limit(500)
        .with_objects::<Vec<u8>>();

    let found = db
        .run(&query)
        .await?
        .committed()
        .expect("every row names an object");
    assert_eq!(found.len(), 3);

    let times: Vec<&[u8]> = found
        .iter()
        .filter_map(|(row, _)| row.key.component(2))
        .collect();
    assert_eq!(times, [b"09:00", b"10:00", b"11:00"]);
    Ok(())
}

/// An index row naming an object that is gone is a reserved abort rather than
/// an error, because the operation did answer.
pub async fn a_dangling_entry_aborts_a_strict_scan<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let file = Operation::new().put_index(&APPTS, key!["dr-a", "d", "t"], ObjectId(404));
    assert!(db.run(&file).await?.is_committed());

    let strict = Query::scan(&APPTS, all())
        .limit(10)
        .with_objects::<Vec<u8>>();
    assert_eq!(
        db.run(&strict).await?,
        Outcome::Aborted(AbortCode::DANGLING_INDEX_ENTRY)
    );

    let lenient = Query::scan(&APPTS, all())
        .limit(10)
        .with_objects_lenient::<Vec<u8>>();
    let rows = db
        .run(&lenient)
        .await?
        .committed()
        .expect("lenient commits");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, None);
    Ok(())
}

/// One step retires a whole run, so a caller never enumerates what it is
/// removing.
pub async fn delete_every_entry_under_a_prefix<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let fill = Operation::new()
        .put_index(&USERS_BY_COUNTRY, key!["py", "1"], ObjectId(1))
        .put_index(&USERS_BY_COUNTRY, key!["py", "2"], ObjectId(2))
        .put_index(&USERS_BY_COUNTRY, key!["ar", "3"], ObjectId(3));
    assert!(db.run(&fill).await?.is_committed());

    let purge = Operation::new().delete_index(&USERS_BY_COUNTRY, prefix(key!["py"]));
    assert!(db.run(&purge).await?.is_committed());

    let left = Query::count(&USERS_BY_COUNTRY, all());
    assert_eq!(db.run(&left).await?, Outcome::Committed(1));
    Ok(())
}

/// Two rules in one operation, and which code comes back is what names the one
/// that refused.
pub async fn uniqueness_across_two_indexes<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;
    let first = ObjectId(1);

    let create = |id: ObjectId, email: &'static str, ci: &'static str| {
        Operation::new()
            .require_absent(&USERS_BY_EMAIL, exact(key![email]), DUPLICATE_EMAIL)
            .require_absent(&USERS_BY_CI, exact(key![ci]), DUPLICATE_CI)
            .put_object(id, b"blob".to_vec())
            .put_index(&USERS_BY_EMAIL, key![email], id)
            .put_index(&USERS_BY_CI, key![ci], id)
            .returning(id)
    };

    assert_eq!(
        db.run(&create(first, "a@b.c", "111")).await?,
        Outcome::Committed(first)
    );
    assert_eq!(
        db.run(&create(ObjectId(2), "a@b.c", "222")).await?,
        Outcome::Aborted(DUPLICATE_EMAIL)
    );
    assert_eq!(
        db.run(&create(ObjectId(3), "c@d.e", "111")).await?,
        Outcome::Aborted(DUPLICATE_CI)
    );
    Ok(())
}

/// Read-your-own-writes, which is the whole reason preconditions and mutations
/// are one ordered list.
pub async fn a_precondition_after_a_mutation_sees_it<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let holds = Operation::new()
        .put_index(&USERS_BY_EMAIL, key!["a@b.c"], ObjectId(1))
        .require_count(&USERS_BY_EMAIL, exact(key!["a@b.c"]), 1, INVARIANT);
    assert!(db.run(&holds).await?.is_committed());

    let blind = Operation::new()
        .put_index(&USERS_BY_CI, key!["111"], ObjectId(1))
        .require_absent(&USERS_BY_CI, exact(key!["111"]), INVARIANT);
    assert_eq!(db.run(&blind).await?, Outcome::Aborted(INVARIANT));

    let untouched = Query::count(&USERS_BY_CI, all());
    assert_eq!(db.run(&untouched).await?, Outcome::Committed(0));
    Ok(())
}

/// Joining two operations adds a unit of work rather than a kind of write, and
/// the abort codes stay distinguishable across the join.
pub async fn two_operations_compose_into_one_transaction<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;
    let id = ObjectId(1);

    let patient = Operation::new()
        .put_object(id, b"patient".to_vec())
        .put_index(&PATIENTS, key!["clinic-a", id], id);
    let account = Operation::new()
        .require_absent(&USERS_BY_CI, exact(key!["111"]), DUPLICATE_CI)
        .put_index(&USERS_BY_CI, key!["111"], id)
        .returning(id);

    assert_eq!(
        db.run(&patient.then(account)).await?,
        Outcome::Committed(id)
    );
    assert_eq!(
        db.run(&Query::count(&PATIENTS, all())).await?,
        Outcome::Committed(1)
    );
    Ok(())
}

/// A precondition on an object, and nothing after it lands when it fails.
pub async fn a_missing_object_refuses_the_whole_operation<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let op = Operation::new()
        .require_object(ObjectId(404), NO_SUCH_USER)
        .put_index(&USERS_BY_EMAIL, key!["a@b.c"], ObjectId(404));
    assert_eq!(db.run(&op).await?, Outcome::Aborted(NO_SUCH_USER));
    assert_eq!(
        db.run(&Query::count(&USERS_BY_EMAIL, all())).await?,
        Outcome::Committed(0)
    );
    Ok(())
}

/// Two reads that have to agree are one request, which is the only thing that
/// makes them trustworthy together.
pub async fn zip_answers_two_reads_from_one_view<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;
    let id = ObjectId(1);

    let fill = Operation::new()
        .put_object(id, b"a blob".to_vec())
        .put_index(&USERS_BY_EMAIL, key!["a@b.c"], id);
    assert!(db.run(&fill).await?.is_committed());

    let both = Query::zip(
        Query::count(&USERS_BY_EMAIL, all()),
        Query::object::<Vec<u8>>(id),
    );
    assert_eq!(
        db.run(&both).await?,
        Outcome::Committed((1, Some(b"a blob".to_vec())))
    );
    Ok(())
}

/// The template is instantiated once per scanned row, with the row's own
/// columns filling its slots.
pub async fn for_each_writes_a_derived_entry_per_row<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let fill = Operation::new()
        .put_index(&PATIENTS, key!["clinic-a", 1u64], ObjectId(1))
        .put_index(&PATIENTS, key!["clinic-a", 2u64], ObjectId(2))
        .put_index(&PATIENTS, key!["clinic-b", 3u64], ObjectId(3));
    assert!(db.run(&fill).await?.is_committed());

    let reindex = Operation::new().for_each(
        Query::scan(&PATIENTS, prefix(key!["clinic-a"])).limit(1000),
        |row| Operation::new().put_index(&PATIENTS_V2, key![row.key(0), row.doc()], row.doc()),
    );
    assert!(db.run(&reindex).await?.is_committed());

    let copied = db
        .run(&Query::scan(&PATIENTS_V2, all()))
        .await?
        .committed()
        .expect("the scan commits");
    let ids: Vec<ObjectId> = copied.iter().map(|row: &Row| row.object).collect();
    assert_eq!(ids, [ObjectId(1), ObjectId(2)]);
    Ok(())
}

/// Caught while building, because it is the one step that can do unbounded
/// work.
pub async fn an_unbounded_for_each_is_refused_before_it_runs<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let reindex = Operation::new().for_each(Query::scan(&PATIENTS, all()), |row| {
        Operation::new().put_index(&PATIENTS_V2, key![row.key(0), row.doc()], row.doc())
    });

    assert!(matches!(
        db.run(&reindex).await,
        Err(StorageError::Invalid(
            doclite_core::BuildError::UnboundedForEach
        ))
    ));
    Ok(())
}

/// A row slot may name a range as well as a key, so a fan-out delete is one
/// step.
pub async fn for_each_deletes_a_range_named_by_each_row<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let fill = Operation::new()
        .put_index(&PATIENTS, key!["clinic-a", 1u64], ObjectId(1))
        .put_index(&PATIENTS, key!["clinic-a", 2u64], ObjectId(2))
        .put_index(&PATIENTS, key!["clinic-b", 3u64], ObjectId(3))
        .put_index(&NOTES_BY_PATIENT, key![ObjectId(1)], ObjectId(11))
        .put_index(&NOTES_BY_PATIENT, key![ObjectId(2)], ObjectId(22))
        .put_index(&NOTES_BY_PATIENT, key![ObjectId(3)], ObjectId(33));
    assert!(db.run(&fill).await?.is_committed());

    let cascade = Operation::new().for_each(
        Query::scan(&PATIENTS, prefix(key!["clinic-a"])).limit(1000),
        |row| Operation::new().delete_index(&NOTES_BY_PATIENT, exact(key![row.doc()])),
    );
    assert!(db.run(&cascade).await?.is_committed());

    let left = db
        .run(&Query::scan(&NOTES_BY_PATIENT, all()))
        .await?
        .committed()
        .expect("the scan commits");
    let ids: Vec<ObjectId> = left.iter().map(|row: &Row| row.object).collect();
    assert_eq!(ids, [ObjectId(33)]);
    Ok(())
}

/// A limit is a ceiling rather than a count, so a scan that finds fewer rows is
/// not a refusal.
pub async fn a_for_each_under_its_limit_still_commits<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let fill = Operation::new()
        .put_index(&PATIENTS, key!["clinic-a", 1u64], ObjectId(1))
        .put_index(&PATIENTS, key!["clinic-a", 2u64], ObjectId(2))
        .put_index(&PATIENTS, key!["clinic-a", 3u64], ObjectId(3));
    assert!(db.run(&fill).await?.is_committed());

    let reindex = Operation::new().for_each(
        Query::scan(&PATIENTS, prefix(key!["clinic-a"])).limit(2),
        |row| Operation::new().put_index(&PATIENTS_V2, key![row.key(0), row.doc()], row.doc()),
    );
    assert!(db.run(&reindex).await?.is_committed());

    let copied = db
        .run(&Query::scan(&PATIENTS_V2, all()))
        .await?
        .committed()
        .expect("the scan commits");
    let ids: Vec<ObjectId> = copied.iter().map(|row: &Row| row.object).collect();
    assert_eq!(ids, [ObjectId(1), ObjectId(2)]);
    Ok(())
}

/// A `for_each` whose scan names no row writes nothing at all.
///
/// The template is instantiated once per row, so no rows means no writes. An
/// engine that writes the whole `for_each` itself has to arrive at the same
/// answer, and a fixed range in a template is where that is easy to lose: the
/// statement it collapses to no longer mentions the scan, and a delete that
/// mentions nothing deletes everything.
/// An empty scan commits, since there was nothing the template was owed.
pub async fn a_for_each_whose_scan_names_no_row_writes_nothing<F: Fresh>() -> Result<(), Failure> {
    let db = open::<F>().await?;

    let fill = Operation::new()
        .put_index(&PATIENTS, key!["clinic-a", 1u64], ObjectId(1))
        .put_index(&NOTES_BY_PATIENT, key![ObjectId(1)], ObjectId(11));
    assert!(db.run(&fill).await?.is_committed());

    let cascade = Operation::new().for_each(
        Query::scan(&PATIENTS, prefix(key!["clinic-nobody"])).limit(1000),
        |_row| Operation::new().delete_index(&NOTES_BY_PATIENT, all()),
    );
    assert!(db.run(&cascade).await?.is_committed());

    let left = db
        .run(&Query::scan(&NOTES_BY_PATIENT, all()))
        .await?
        .committed()
        .expect("the scan commits");
    let ids: Vec<ObjectId> = left.iter().map(|row: &Row| row.object).collect();
    assert_eq!(ids, [ObjectId(11)], "a scan of no rows retires no row");
    Ok(())
}

/// A catalog that names one index two ways is refused where it is declared.
///
/// Not an engine's rule, but every engine is opened through this, and the
/// mistake is one a caller makes by opening two records that happen to agree on
/// a name. Left to run, an engine keeps the last declaration and a row filed
/// against the other fails later as an arity nobody wrote.
/// Two declarations that disagree are caught at `open`, where the disagreement
/// is still legible.
pub async fn a_catalog_that_names_an_index_twice_is_refused<F: Fresh>() -> Result<(), Failure> {
    let narrow: Index<1> = Index::typed("testkit_two_ways", &[ColumnType::Str]);
    let wide: Index<2> = Index::typed("testkit_two_ways", &[ColumnType::Str, ColumnType::U64]);

    let refused = Db::open(F::fresh().await?, &[narrow.schema(), wide.schema()]).await;
    let Err(StorageError::Redeclared { index, .. }) = refused else {
        panic!("a catalog cannot name one index two ways");
    };
    assert_eq!(index, narrow.schema().id, "it names the index it refused");

    // The same declaration twice is ordinary, not a mistake.
    Db::open(F::fresh().await?, &[narrow.schema(), narrow.schema()]).await?;
    Ok(())
}
