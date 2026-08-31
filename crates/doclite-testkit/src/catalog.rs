//! The indexes every case is written against.
//!
//! Both halves of the suite share them: a case files rows into them through the
//! engine directly, and the transcript script drives a `Db` over the same two.

use doclite_core::{AbortCode, Index, IndexSchema};

pub(crate) static BY_EMAIL: Index<1> = Index::new("testkit_by_email");
pub(crate) static BY_PAIR: Index<2> = Index::new("testkit_by_pair");

/// What the script's refused precondition reports. Well clear of the codes
/// `doclite-core` reserves for itself.
pub(crate) const REFUSED: AbortCode = AbortCode::new(900);

pub(crate) fn schemas() -> [IndexSchema; 2] {
    [BY_EMAIL.schema(), BY_PAIR.schema()]
}
