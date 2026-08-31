//! Rules about the executor, checked by running the same work two ways.
//!
//! Not the same question the cases ask. A case checks what an engine owes; this
//! checks that the executor is leaning on the engine for speed rather than for
//! correctness, which no single run can show.

use doclite_core::{
    all, exact, key, prefix, Db, Engine, ObjectId, Operation, Outcome, Query, StorageError,
};

use crate::catalog::{schemas, BY_EMAIL, BY_PAIR, REFUSED};
use crate::failure::{expect, Failure};
use crate::fresh::Fresh;
use crate::probe::Probe;

/// One line of a transcript: what the script did, and what came back.
type Step = (&'static str, String);

/// Runs one script against an engine and against the same engine with its hints
/// stripped, and demands the same transcript.
pub async fn hints_are_hints<F: Fresh>() -> Result<(), Failure> {
    let bare = Db::open(F::fresh().await?, &schemas()).await?;
    let probed = Db::open(Probe::new(F::fresh().await?), &schemas()).await?;

    let left = script(&bare).await?;
    let right = script(&probed).await?;

    agree(&left, &right)?;
    expect(
        probed.engine().reads() > 0,
        "the probe was never asked anything",
    )
}

/// Names the first step the two runs disagreed on, rather than printing both
/// transcripts whole and leaving the reader to find it.
fn agree(bare: &[Step], probed: &[Step]) -> Result<(), Failure> {
    for ((step, left), (_, right)) in bare.iter().zip(probed) {
        expect(
            left == right,
            format!("{step}: bare {left}, probed {right}"),
        )?;
    }
    expect(
        bare.len() == probed.len(),
        format!("bare ran {} steps, probed ran {}", bare.len(), probed.len()),
    )
}

/// Enough of the API that every hint gets a chance to matter.
async fn script<E: Engine>(db: &Db<E>) -> Result<Vec<Step>, StorageError> {
    let mut transcript = Vec::new();

    let seed = Operation::new()
        .put_object(ObjectId(1), b"one".to_vec())
        .put_object(ObjectId(2), b"two".to_vec())
        .put_index(&BY_EMAIL, key!["a@b.c"], ObjectId(1))
        .put_index(&BY_EMAIL, key!["c@d.e"], ObjectId(2))
        .put_index(&BY_PAIR, key!["clinic", 1u64], ObjectId(1))
        .put_index(&BY_PAIR, key!["clinic", 2u64], ObjectId(2))
        .put_index(&BY_PAIR, key!["other", 3u64], ObjectId(2));
    transcript.push(("seed the store", format!("{:?}", db.run(&seed).await?)));

    transcript.push((
        "count an exact key",
        format!(
            "{:?}",
            db.run(&Query::count(&BY_EMAIL, exact(key!["a@b.c"])))
                .await?
        ),
    ));
    transcript.push((
        "count a prefix",
        format!(
            "{:?}",
            db.run(&Query::count(&BY_PAIR, prefix(key!["clinic"])))
                .await?
        ),
    ));
    transcript.push((
        "scan under a limit",
        format!(
            "{:?}",
            db.run(&Query::scan(&BY_PAIR, all()).limit(2)).await?
        ),
    ));
    transcript.push((
        "scan with objects",
        format!(
            "{:?}",
            db.run(&Query::scan(&BY_EMAIL, all()).with_objects::<Vec<u8>>())
                .await?
        ),
    ));
    transcript.push((
        "zip a count and an object",
        format!(
            "{:?}",
            db.run(&Query::zip(
                Query::count(&BY_EMAIL, all()),
                Query::object::<Vec<u8>>(ObjectId(2))
            ))
            .await?
        ),
    ));

    let taken = Operation::new().require_absent(&BY_EMAIL, exact(key!["a@b.c"]), REFUSED);
    transcript.push((
        "require_absent on a taken key",
        format!("{:?}", db.run(&taken).await?),
    ));

    let reindex = Operation::new().for_each(Query::scan(&BY_EMAIL, all()).limit(100), |row| {
        Operation::new().put_index(&BY_PAIR, key![row.key(0), row.doc()], row.doc())
    });
    transcript.push((
        "a for_each as the whole operation",
        format!("{:?}", db.run(&reindex).await?),
    ));
    transcript.push((
        "scan what the for_each derived",
        format!("{:?}", db.run(&Query::scan(&BY_PAIR, all())).await?),
    ));

    let purge = Operation::new().delete_index(&BY_PAIR, prefix(key!["clinic"]));
    transcript.push((
        "delete a whole prefix",
        format!("{:?}", db.run(&purge).await?),
    ));
    transcript.push((
        "scan after the delete",
        format!("{:?}", db.run(&Query::scan(&BY_PAIR, all())).await?),
    ));

    let outcome: Outcome<()> = db.run(&Operation::new()).await?;
    transcript.push(("run an empty operation", format!("{outcome:?}")));

    Ok(transcript)
}
