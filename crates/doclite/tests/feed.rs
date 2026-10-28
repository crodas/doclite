//! What a record that declares an owner is filed under.
//!
//! The index is what the feature is: an owner's changes are rows in one
//! ordered index, so what has to be true of them is what has to be true of
//! `deal_feed`.

use doclite::{Column, Document, Record};
use doclite_core::{ColumnType, IndexId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Column)]
struct Reference(String);

/// A deal owned by the account it belongs to, and indexed by it as well, which
/// is the pair `#[owner]` beside `#[index]` is for: one answers who it is
/// about now, the other what happened to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Deal {
    #[unique]
    reference: Reference,

    #[owner]
    #[index]
    account: String,

    amount_cents: i64,
}

#[test]
fn an_owner_declares_a_feed_beside_the_record_s_other_indexes() {
    let named: Vec<IndexId> = Deal::schemas().iter().map(|schema| schema.id).collect();
    assert_eq!(
        named,
        [
            IndexId("deal_latest"),
            IndexId("deal_by_reference"),
            IndexId("deal_feed"),
            IndexId("deal_by_account"),
        ]
    );
}

/// The owner, then the revision, then the document. The revision in the middle
/// is what makes a scan of one owner read in the order its changes happened.
#[test]
fn a_feed_is_keyed_on_the_owner_the_revision_and_the_document() {
    let feed = Deal::schemas()
        .into_iter()
        .find(|schema| schema.id == IndexId("deal_feed"))
        .expect("the feed is declared");

    assert_eq!(
        feed.columns,
        [ColumnType::Str, ColumnType::I64, ColumnType::I64],
        "an id is declared as the signed integer it round-trips through"
    );
}

/// A record owned by several at once: the users watching a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Document)]
struct Task {
    #[unique]
    title: String,

    #[owner]
    watchers: Vec<String>,
}

/// Several owners do not widen the key: a column holds one owner, and a feed is
/// read for one owner whichever shape the field has. What changes is how many
/// rows one revision files, which is one per owner.
#[test]
fn a_feed_over_several_owners_is_keyed_on_one_of_them() {
    let feed = Task::schemas()
        .into_iter()
        .find(|schema| schema.id == IndexId("task_feed"))
        .expect("the feed is declared");

    assert_eq!(
        feed.columns,
        [ColumnType::Str, ColumnType::I64, ColumnType::I64],
        "the element answers for the column, not the list"
    );
}
