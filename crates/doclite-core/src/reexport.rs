//! What a codec macro needs to name from the crate it expands in.
//!
//! The macro's caller does not depend on the codec, so every path it writes has
//! to go back through this crate.

#[cfg(feature = "cbor")]
pub use ciborium;

#[cfg(feature = "json")]
pub use serde_json;
