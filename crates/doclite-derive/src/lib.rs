//! The derive macros for DocLite's document layer.

mod column;
mod document;
mod parse;
mod spec;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Makes a type usable as an index column.
///
/// A newtype delegates to the value it wraps. A fieldless enum encodes the
/// variant's name, which is what keeps reordering variants from silently
/// changing what every existing index row means.
#[proc_macro_derive(Column)]
pub fn derive_column(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    column::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Turns a struct into its own indexes, queries and revision history.
///
/// ```text
/// #[derive(Debug, Clone, Serialize, Deserialize, Document)]
/// #[index(country, city, state)]
/// pub struct User {
///     #[unique] email: Email,
///     country: Country,
///     city: String,
///     state: Option<String>,
///     #[index] age: u8,
///     accepted_tos: bool,
/// }
/// ```
///
/// `#[owner]` on a field declares a feed: one row per revision rather than per
/// record, keyed `(owner, revision, document)`, which is what lets one owner's
/// changes be read in the order they happened.
///
/// ```text
/// #[derive(Debug, Clone, Serialize, Deserialize, Document)]
/// pub struct Deal {
///     #[owner] account: Ref<Account>,
///     #[index] amount_cents: i64,
/// }
/// ```
///
/// A struct marked `#[immutable]` is filed and queried like any other, but the
/// expansion leaves out the impl that `Connection::update` asks for, so editing
/// one is a compile error:
///
/// ```text
/// #[derive(Debug, Clone, Serialize, Deserialize, Document)]
/// #[immutable]
/// #[index(account, at)]
/// pub struct LedgerEntry {
///     account: Ref<Account>,
///     at: Timestamp,
///     amount: i64,
/// }
/// ```
///
/// Shown rather than compiled, because this crate cannot depend on the one that
/// gives the expansion something to stand on. `doclite`'s own documentation
/// carries the same example as a test that runs.
#[proc_macro_derive(Document, attributes(index, unique, owner, immutable, storage))]
pub fn derive_document(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    document::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
