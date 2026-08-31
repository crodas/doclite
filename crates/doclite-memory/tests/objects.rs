//! Objects that are values rather than bytes.

use doclite_core::{
    all, exact, key, prefix, AbortCode, BuildError, CodecError, Db, Index, ObjectId, Operation,
    Outcome, Query, StorageError, ToStorage,
};
use doclite_memory::MemoryEngine;
use serde::{Deserialize, Serialize};

static USERS_BY_EMAIL: Index<1> = Index::new("users_by_email");

const DUPLICATE_EMAIL: AbortCode = AbortCode::new(100);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct User {
    email: String,
    visits: u32,
}

doclite_core::serde_storage!(User);

/// The same record after a field was added, standing in for a type that changed
/// after objects were already written under the old shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UserV2 {
    email: String,
    visits: u32,
    #[serde(default)]
    nickname: Option<String>,
}

doclite_core::serde_storage!(UserV2);

async fn open() -> Db<MemoryEngine> {
    Db::open(MemoryEngine::new(), &[USERS_BY_EMAIL.schema()])
        .await
        .expect("the engine accepts its own schemas")
}

fn ana() -> User {
    User {
        email: "a@b.c".into(),
        visits: 3,
    }
}

#[tokio::test]
async fn a_record_goes_in_and_comes_back_as_itself() -> Result<(), StorageError> {
    let db = open().await;
    let id = ObjectId(1);

    let op = Operation::new()
        .require_absent(&USERS_BY_EMAIL, exact(key!["a@b.c"]), DUPLICATE_EMAIL)
        .put_object(id, ana())
        .put_index(&USERS_BY_EMAIL, key!["a@b.c"], id)
        .returning(id);
    assert_eq!(db.run(&op).await?, Outcome::Committed(id));

    let read = Query::object::<User>(id);
    assert_eq!(db.run(&read).await?, Outcome::Committed(Some(ana())));
    Ok(())
}

#[tokio::test]
async fn a_scan_decodes_every_object_it_fetched() -> Result<(), StorageError> {
    let db = open().await;

    let mut fill = Operation::new();
    for (at, email) in ["a@b.c", "d@e.f"].iter().enumerate() {
        let id = ObjectId(at as u64 + 1);
        let user = User {
            email: (*email).into(),
            visits: at as u32,
        };
        fill = fill
            .put_object(id, user)
            .put_index(&USERS_BY_EMAIL, key![*email], id);
    }
    assert!(db.run(&fill).await?.is_committed());

    let scan = Query::scan(&USERS_BY_EMAIL, all())
        .limit(10)
        .with_objects::<User>();
    let found = db.run(&scan).await?.committed().expect("every row has one");

    let emails: Vec<&str> = found.iter().map(|(_, user)| user.email.as_str()).collect();
    assert_eq!(emails, ["a@b.c", "d@e.f"]);
    Ok(())
}

/// The property that decided the format. An object is never rewritten, so one
/// written under the old shape has to still read under the new one.
#[tokio::test]
async fn a_object_written_before_a_field_existed_still_reads() -> Result<(), StorageError> {
    let db = open().await;
    let id = ObjectId(1);

    let op = Operation::new().put_object(id, ana());
    assert!(db.run(&op).await?.is_committed());

    let read = Query::object::<UserV2>(id);
    assert_eq!(
        db.run(&read).await?,
        Outcome::Committed(Some(UserV2 {
            email: "a@b.c".into(),
            visits: 3,
            nickname: None,
        }))
    );
    Ok(())
}

/// `user` is borrowed rather than moved here, which is the case the `&T` impl
/// exists for: the caller still needs the value after filing it.
#[tokio::test]
async fn bytes_and_records_share_one_store() -> Result<(), StorageError> {
    let db = open().await;

    let user = ana();
    let op = Operation::new()
        .put_object(ObjectId(1), b"a blob".to_vec())
        .put_object(ObjectId(2), &user);
    assert!(db.run(&op).await?.is_committed());

    let raw = Query::object::<Vec<u8>>(ObjectId(1));
    assert_eq!(
        db.run(&raw).await?,
        Outcome::Committed(Some(b"a blob".to_vec()))
    );

    let typed = Query::object::<User>(ObjectId(2));
    assert_eq!(db.run(&typed).await?, Outcome::Committed(Some(user)));
    Ok(())
}

/// Bytes that were never a `User` are a decode failure rather than a wrong
/// answer, and it names the codec rather than the store.
#[tokio::test]
async fn the_wrong_type_is_a_decode_failure() -> Result<(), StorageError> {
    let db = open().await;
    let id = ObjectId(1);

    let op = Operation::new().put_object(id, b"not cbor at all".to_vec());
    assert!(db.run(&op).await?.is_committed());

    let read = Query::object::<User>(id);
    assert!(matches!(
        db.run(&read).await,
        Err(StorageError::Decode(CodecError::Decoding(_)))
    ));
    Ok(())
}

/// A type that cannot encode is a defect in the operation, so it is refused
/// before an engine is asked anything.
#[tokio::test]
async fn a_value_that_cannot_encode_refuses_the_operation() -> Result<(), StorageError> {
    let db = open().await;

    struct Unwritable;

    impl ToStorage for Unwritable {
        fn to_storage(&self) -> Result<Vec<u8>, CodecError> {
            Err(CodecError::encoding("this one never encodes"))
        }
    }

    let op = Operation::new()
        .put_object(ObjectId(1), Unwritable)
        .put_index(&USERS_BY_EMAIL, key!["a@b.c"], ObjectId(1));

    assert!(matches!(
        db.run(&op).await,
        Err(StorageError::Invalid(BuildError::Encode(_)))
    ));

    let untouched = Query::count(&USERS_BY_EMAIL, all());
    assert_eq!(db.run(&untouched).await?, Outcome::Committed(0));
    Ok(())
}

#[tokio::test]
async fn a_for_each_over_records_still_works() -> Result<(), StorageError> {
    let db = open().await;

    let fill = Operation::new().put_object(ObjectId(1), ana()).put_index(
        &USERS_BY_EMAIL,
        key!["a@b.c"],
        ObjectId(1),
    );
    assert!(db.run(&fill).await?.is_committed());

    let purge = Operation::new().for_each(
        Query::scan(&USERS_BY_EMAIL, prefix(key!["a@b.c"])).limit(100),
        |row| Operation::new().delete_index(&USERS_BY_EMAIL, exact(key![row.key(0)])),
    );
    assert!(db.run(&purge).await?.is_committed());

    assert_eq!(
        db.run(&Query::count(&USERS_BY_EMAIL, all())).await?,
        Outcome::Committed(0)
    );
    Ok(())
}
