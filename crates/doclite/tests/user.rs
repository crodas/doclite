//! The API from the outside, written the way a caller writes it.

use doclite::doclite_core::{ColumnType, ObjectId};
use std::sync::atomic::{AtomicU64, Ordering};

use doclite::id::{minted_at, EPOCH_MS};
use doclite::{
    greater_than, Clock, Column, Connection, Document, DocumentError, DocumentId, FixedClock,
    NodeId, Record, Revision, RevisionId, Timestamp,
};
use serde::{Deserialize, Serialize};

mod under;
use under::{on_every_engine, Under};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Email(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Column)]
enum Country {
    Argentina,
    Paraguay,
}

use Country::{Argentina, Paraguay};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
#[index(country, city, state)]
struct User {
    #[unique]
    email: Email,

    country: Country,

    city: String,

    state: Option<String>,

    #[index]
    age: u8,

    accepted_tos: bool,
}

/// Records what a record's walk yields, so a test can read it.
#[derive(Default)]
struct Walk(Vec<(&'static str, bool)>);

impl doclite::Rows for Walk {
    fn row<const N: usize>(
        &mut self,
        _index: &doclite::doclite_core::Index<N>,
        _key: doclite::doclite_core::Key<N>,
        name: &'static str,
        unique: bool,
    ) {
        self.0.push((name, unique));
    }
}

fn clock() -> FixedClock {
    FixedClock(Timestamp(EPOCH_MS + 1_000))
}

async fn open<U: Under>() -> Connection<U::Engine, FixedClock> {
    Connection::with_clock(
        U::engine().await,
        &User::schemas(),
        clock(),
        NodeId::new(7).expect("seven fits in ten bits"),
    )
    .await
    .expect("the engine accepts its own schemas")
}

fn user(email: &str, country: Country, city: &str, age: u8) -> User {
    User {
        email: Email(email.into()),
        country,
        city: city.into(),
        state: None,
        age,
        accepted_tos: true,
    }
}

async fn the_example_from_the_readme<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;

    let x = user("a@b.c", Paraguay, "Asuncion", 24);
    conn.insert(x).await?;

    conn.insert(user("d@e.f", Paraguay, "Asuncion", 30)).await?;
    conn.insert(user("g@h.i", Argentina, "Cordoba", 41)).await?;

    let adults = User::query(&conn).by_age(18..).limit(10).get_many().await?;
    assert_eq!(adults.len(), 3);

    let paraguayans = User::query(&conn)
        .by_country(Paraguay)
        .limit(10)
        .get_many()
        .await?;
    assert_eq!(paraguayans.len(), 2);

    let asuncion = User::query(&conn)
        .by_country_city(Paraguay, "Asuncion".to_string())
        .limit(10)
        .get_many()
        .await?;
    assert_eq!(asuncion.len(), 2);

    let mut user = User::query(&conn)
        .by_email(Email("a@b.c".into()))
        .get_one()
        .await?;

    user.age = 25;
    conn.update(user).await?;

    let now = User::query(&conn)
        .by_email(Email("a@b.c".into()))
        .get_one()
        .await?;
    assert_eq!(now.age, 25);
    Ok(())
}

async fn the_derive_makes_the_indexes_the_attributes_asked_for<U: Under>(
) -> Result<(), DocumentError> {
    let names: Vec<&str> = User::schemas().iter().map(|schema| schema.id.0).collect();
    assert_eq!(
        names,
        [
            "user_latest",
            "user_by_email",
            "user_by_age",
            "user_by_country_city_state"
        ],
        "every record is filed under its own identity first"
    );

    let arities: Vec<usize> = User::schemas()
        .iter()
        .map(|schema| schema.arity())
        .collect();
    assert_eq!(
        arities,
        [2, 1, 2, 4],
        "the generated index is (document, revision); only a unique one skips the id"
    );

    let mut walked = Walk::default();
    user("a@b.c", Paraguay, "Asuncion", 24).rows(
        DocumentId(ObjectId(1)),
        RevisionId(ObjectId(1)),
        &mut walked,
    );
    assert_eq!(
        walked.0,
        [
            ("user_latest", false),
            ("user_by_email", true),
            ("user_by_age", false),
            ("user_by_country_city_state", false),
        ],
        "one row per index, and only the caller's own is unique"
    );

    assert_eq!(User::LATEST.id().0, "user_latest");

    assert_eq!(User::BY_EMAIL.id().0, "user_by_email");
    assert_eq!(User::BY_AGE.id().0, "user_by_age");
    assert_eq!(
        User::BY_COUNTRY_CITY_STATE.id().0,
        "user_by_country_city_state"
    );

    Ok(())
}

/// A store that lays a column out by type gets one, and everything the derive
/// touches on the way answers for itself: a newtype through what it wraps, an
/// enum as the variant name it encodes, an `Option` as nullable, and the
/// trailing column as the document id it is.
///
/// Every id column is `I64` rather than `U64`, which is `ObjectId` reserving
/// its top bit so a backend with signed integers can hold one as an integer.
#[test]
fn the_derive_says_what_each_column_holds() {
    let columns: Vec<&[ColumnType]> = User::schemas()
        .iter()
        .map(|schema| schema.columns)
        .collect();

    assert_eq!(
        columns,
        [
            &[ColumnType::I64, ColumnType::I64][..],
            &[ColumnType::Str][..],
            &[ColumnType::U8, ColumnType::I64][..],
            &[
                ColumnType::Str,
                ColumnType::Str,
                ColumnType::Nullable(&ColumnType::Str),
                ColumnType::I64,
            ][..],
        ]
    );
}

/// A type nobody described is bytes rather than a refusal, which is what keeps
/// the column type from being a thing every caller has to learn.
#[test]
fn a_type_with_nothing_said_about_it_is_bytes() {
    struct Opaque;

    impl doclite::doclite_core::ToOrdBytes for Opaque {
        fn to_ord_bytes(&self) -> Vec<u8> {
            Vec::new()
        }
    }

    assert_eq!(
        <Opaque as doclite::doclite_core::ToOrdBytes>::COLUMN_TYPE,
        ColumnType::Bytes
    );
}

async fn every_comparison_form_selects_the_right_rows<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    for (at, age) in [10u8, 18, 25, 40, 65].into_iter().enumerate() {
        conn.insert(user(&format!("{at}@b.c"), Paraguay, "Asuncion", age))
            .await?;
    }

    macro_rules! ages {
        ($bounds:expr) => {{
            let mut found: Vec<u8> = User::query(&conn)
                .by_age($bounds)
                .get_many()
                .await?
                .iter()
                .map(|user| user.age)
                .collect();
            found.sort_unstable();
            found
        }};
    }

    assert_eq!(ages!(25), [25]);
    assert_eq!(ages!(18..), [18, 25, 40, 65]);
    assert_eq!(ages!(..25), [10, 18]);
    assert_eq!(ages!(..=25), [10, 18, 25]);
    assert_eq!(ages!(18..40), [18, 25]);
    assert_eq!(ages!(18..=40), [18, 25, 40]);
    assert_eq!(ages!(greater_than(25)), [40, 65]);
    Ok(())
}

async fn a_compound_index_answers_every_prefix_of_itself<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let mut with_state = user("a@b.c", Paraguay, "Asuncion", 30);
    with_state.state = Some("Central".into());
    conn.insert(with_state).await?;
    conn.insert(user("d@e.f", Paraguay, "Asuncion", 31)).await?;
    conn.insert(user("g@h.i", Paraguay, "Encarnacion", 32))
        .await?;
    conn.insert(user("j@k.l", Argentina, "Asuncion", 33))
        .await?;

    assert_eq!(
        User::query(&conn)
            .by_country(Paraguay)
            .get_many()
            .await?
            .len(),
        3
    );
    assert_eq!(
        User::query(&conn)
            .by_country_city(Paraguay, "Asuncion".to_string())
            .get_many()
            .await?
            .len(),
        2
    );
    assert_eq!(
        User::query(&conn)
            .by_country_city_state(
                Paraguay,
                "Asuncion".to_string(),
                Some("Central".to_string())
            )
            .get_many()
            .await?
            .len(),
        1
    );
    Ok(())
}

async fn a_range_on_the_last_component_of_a_compound_index<U: Under>() -> Result<(), DocumentError>
{
    let conn = open::<U>().await;
    for city in ["Asuncion", "Encarnacion", "Luque", "Villarrica"] {
        conn.insert(user(&format!("{city}@b.c"), Paraguay, city, 30))
            .await?;
    }

    let early = User::query(&conn)
        .by_country_city(Paraguay, "A".to_string().."M".to_string())
        .get_many()
        .await?;
    let cities: Vec<&str> = early.iter().map(|user| user.city.as_str()).collect();
    assert_eq!(cities, ["Asuncion", "Encarnacion", "Luque"]);
    Ok(())
}

async fn an_update_supersedes_and_moves_every_index<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    let first = conn.insert(user("a@b.c", Paraguay, "Asuncion", 24)).await?;

    let mut editing = User::query(&conn).by_age(24).get_one().await?;
    editing.age = 25;
    editing.city = "Encarnacion".into();
    let second = conn.update(editing).await?;

    assert_eq!(second.prev(), Some(first.revision()));
    assert_ne!(second.revision(), first.revision());
    assert_eq!(
        second.id(),
        first.id(),
        "a supersede is still the same record"
    );

    assert!(User::query(&conn).by_age(24).get_many().await?.is_empty());
    assert_eq!(
        User::query(&conn).by_age(25).get_one().await?.id(),
        second.id()
    );
    assert_eq!(
        User::query(&conn)
            .by_country_city(Paraguay, "Asuncion".to_string())
            .get_many()
            .await?
            .len(),
        0
    );
    assert_eq!(
        User::query(&conn)
            .by_email(Email("a@b.c".into()))
            .get_one()
            .await?
            .id(),
        second.id()
    );
    Ok(())
}

async fn history_walks_back_through_every_revision<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(user("a@b.c", Paraguay, "Asuncion", 20)).await?;

    let mut latest = None;
    for age in [21u8, 22, 23] {
        let mut editing = User::query(&conn).by_age(age - 1).get_one().await?;
        editing.age = age;
        latest = Some(conn.update(editing).await?);
    }

    let latest = latest.expect("three updates ran");
    let ages: Vec<u8> = conn
        .history(&latest)
        .await?
        .iter()
        .map(|user| user.age)
        .collect();
    assert_eq!(ages, [22, 21, 20]);
    Ok(())
}

async fn a_stale_edit_loses_rather_than_clobbering<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(user("a@b.c", Paraguay, "Asuncion", 30)).await?;

    let mut mine = User::query(&conn).by_age(30).get_one().await?;
    let mut theirs = User::query(&conn).by_age(30).get_one().await?;

    theirs.city = "Encarnacion".into();
    conn.update(theirs).await?;

    mine.city = "Ciudad del Este".into();
    assert_eq!(conn.update(mine).await, Err(DocumentError::Stale));

    assert_eq!(
        User::query(&conn).by_age(30).get_one().await?.city,
        "Encarnacion"
    );
    Ok(())
}

async fn a_taken_unique_key_names_the_index_and_stages_nothing<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(user("a@b.c", Paraguay, "Asuncion", 30)).await?;

    assert_eq!(
        conn.insert(user("a@b.c", Argentina, "Cordoba", 40)).await,
        Err(DocumentError::Duplicate {
            index: "user_by_email"
        })
    );
    assert!(User::query(&conn).by_age(40).get_many().await?.is_empty());
    Ok(())
}

/// An update that does not touch the unique field must not trip over its own
/// key, which is why the check runs after the old rows are retired.
async fn an_update_that_keeps_its_unique_key_is_not_a_duplicate<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(user("a@b.c", Paraguay, "Asuncion", 30)).await?;

    let mut editing = User::query(&conn).by_age(30).get_one().await?;
    editing.city = "Luque".into();
    let moved = conn.update(editing).await?;
    assert_eq!(moved.email, Email("a@b.c".into()));
    Ok(())
}

async fn an_update_onto_a_taken_unique_key_is_refused<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(user("a@b.c", Paraguay, "Asuncion", 30)).await?;
    conn.insert(user("d@e.f", Paraguay, "Asuncion", 31)).await?;

    let mut editing = User::query(&conn).by_age(31).get_one().await?;
    editing.email = Email("a@b.c".into());
    assert_eq!(
        conn.update(editing).await,
        Err(DocumentError::Duplicate {
            index: "user_by_email"
        })
    );

    let untouched = User::query(&conn).by_age(31).get_one().await?;
    assert_eq!(
        untouched.email,
        Email("d@e.f".into()),
        "and it kept its own"
    );
    Ok(())
}

async fn get_one_refuses_to_pick_a_winner<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(user("a@b.c", Paraguay, "Asuncion", 30)).await?;
    conn.insert(user("d@e.f", Paraguay, "Asuncion", 31)).await?;

    assert_eq!(
        User::query(&conn).by_country(Paraguay).get_one().await,
        Err(DocumentError::NotUnique)
    );
    Ok(())
}

async fn a_query_naming_no_index_is_refused<U: Under>() -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    assert_eq!(
        User::query(&conn).get_many().await,
        Err(DocumentError::NoIndex)
    );

    Ok(())
}

/// A clock that moves on every read, which is what a real one does between two
/// calls a few microseconds apart.
#[derive(Clone, Copy)]
struct MovingClock(&'static AtomicU64);

impl Clock for MovingClock {
    fn now(&self) -> Timestamp {
        Timestamp(EPOCH_MS + self.0.fetch_add(1, Ordering::Relaxed))
    }
}

static TICKS: AtomicU64 = AtomicU64::new(1_000);

/// ADR 0009 says the id and `created_at` come from one read. `Connection` used
/// to hold a clock of its own beside the mint's and read both, so under a
/// moving clock a revision could carry a timestamp from a different millisecond
/// than the one its own id encodes. Every other test pins the clock, which is
/// exactly why nothing caught it.
async fn a_revision_is_stamped_with_the_millisecond_its_id_encodes<U: Under>(
) -> Result<(), DocumentError> {
    let conn = Connection::with_clock(
        U::engine().await,
        &User::schemas(),
        MovingClock(&TICKS),
        NodeId::new(7).expect("seven fits in ten bits"),
    )
    .await?;

    let stored = conn.insert(user("a@b.c", Paraguay, "Asuncion", 30)).await?;
    assert_eq!(stored.created_at(), minted_at(stored.revision()));

    let mut edited = stored;
    edited.age = 31;
    let updated = conn.update(edited).await?;
    assert_eq!(updated.created_at(), minted_at(updated.revision()));
    Ok(())
}

async fn a_pinned_clock_makes_two_runs_produce_the_same_bytes<U: Under>(
) -> Result<(), DocumentError> {
    let one = open::<U>().await;
    let two = open::<U>().await;
    let value = user("a@b.c", Paraguay, "Asuncion", 30);

    let left = one.insert(value.clone()).await?;
    let right = two.insert(value).await?;

    assert_eq!(left.id(), right.id());
    assert_eq!(left.created_at(), clock().now());

    let bytes = |stored: &doclite::Stored<User>| {
        use doclite::doclite_core::ToStorage;
        Revision::new(
            (**stored).clone(),
            stored.id(),
            stored.created_at(),
            stored.prev(),
        )
        .to_storage()
        .expect("a user encodes")
    };
    assert_eq!(bytes(&left), bytes(&right));
    Ok(())
}

/// A variant encodes as its name, so reordering the enum cannot silently change
/// what an index row written yesterday means.
#[test]
fn an_enum_column_encodes_its_variant_name() {
    use doclite::doclite_core::ToOrdBytes;
    assert_eq!(Paraguay.to_ord_bytes(), b"Paraguay".to_vec());
    assert_eq!(Argentina.to_ord_bytes(), b"Argentina".to_vec());
}

#[test]
fn a_newtype_column_encodes_what_it_wraps() {
    use doclite::doclite_core::ToOrdBytes;
    assert_eq!(Email("a@b.c".into()).to_ord_bytes(), "a@b.c".to_ord_bytes());
}

/// A range over the last component of a compound index must keep the ones
/// pinned before it.
///
/// An open end used to be no bound at all, so `by_country_city(Paraguay, ..x)`
/// dropped the country from the low end and answered with every country sorting
/// below it. On an index led by a status that is a query for closed work
/// answering with open work, which is why this is a correctness test rather
/// than a tidiness one.
async fn an_open_end_stays_inside_the_components_pinned_before_it<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(user("first@example.com", Argentina, "Cordoba", 30))
        .await
        .expect("filed");
    conn.insert(user("second@example.com", Paraguay, "Asuncion", 30))
        .await
        .expect("filed");

    let below = User::query(&conn)
        .by_country_city(Paraguay, .."Zzz".to_string())
        .get_many()
        .await
        .expect("a scan");
    assert_eq!(below.len(), 1, "an open low end must not leave Paraguay");
    assert_eq!(below[0].country, Paraguay);

    let above = User::query(&conn)
        .by_country_city(Argentina, "A".to_string()..)
        .get_many()
        .await
        .expect("a scan");
    assert_eq!(above.len(), 1, "an open high end must not leave Argentina");
    assert_eq!(above[0].country, Argentina);

    Ok(())
}

/// With nothing pinned, an open end really is the end of the index.
async fn an_open_end_on_the_leading_component_is_still_unbounded<U: Under>(
) -> Result<(), DocumentError> {
    let conn = open::<U>().await;
    conn.insert(user("young@example.com", Paraguay, "Asuncion", 20))
        .await
        .expect("filed");
    conn.insert(user("older@example.com", Paraguay, "Asuncion", 60))
        .await
        .expect("filed");

    let everyone = User::query(&conn)
        .by_age(..u8::MAX)
        .get_many()
        .await
        .expect("a scan");
    assert_eq!(everyone.len(), 2);

    Ok(())
}

on_every_engine! {
    #[tokio::test]
    the_example_from_the_readme,
    the_derive_makes_the_indexes_the_attributes_asked_for,
    every_comparison_form_selects_the_right_rows,
    a_compound_index_answers_every_prefix_of_itself,
    a_range_on_the_last_component_of_a_compound_index,
    an_update_supersedes_and_moves_every_index,
    history_walks_back_through_every_revision,
    a_stale_edit_loses_rather_than_clobbering,
    a_taken_unique_key_names_the_index_and_stages_nothing,
    an_update_that_keeps_its_unique_key_is_not_a_duplicate,
    an_update_onto_a_taken_unique_key_is_refused,
    get_one_refuses_to_pick_a_winner,
    a_query_naming_no_index_is_refused,
    a_revision_is_stamped_with_the_millisecond_its_id_encodes,
    a_pinned_clock_makes_two_runs_produce_the_same_bytes,
    an_open_end_stays_inside_the_components_pinned_before_it,
    an_open_end_on_the_leading_component_is_still_unbounded,
}
