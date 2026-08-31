//! The words every reader of this crate shares.
//!
//! An object is named by an id, an index is a name over a fixed number of
//! columns, a key is columns encoded so their bytes sort the way the values do,
//! and a range names some of an index's rows. Nothing here knows how work runs
//! or what an engine is.

pub(crate) mod column;
pub(crate) mod encoding;
pub(crate) mod index;
pub(crate) mod key;
pub(crate) mod object;
pub(crate) mod range;
pub(crate) mod row;
pub(crate) mod storage;
