//! What a declared index is, and everything that follows from what it is for.
//!
//! `Kind` decides three things at once: whether the key carries a trailing
//! document id, whether an insert asserts absence on it, and whether it owns a
//! duplicate code. Every answer that depends on it is here, so ADR 0009's
//! forced choice about the trailing column is stated once rather than in each
//! file that happens to need it.
//!
//! `parse` reads attributes into these; `document` emits from them.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Ident, Type};

/// What an index is for, which decides three separate things that used to ride
/// on one flag: whether the key carries a trailing document id, whether an
/// insert asserts absence on it, and whether it owns a duplicate code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `#[index]`. Many rows may share the key, so the key ends in the document
    /// id to keep one record's row retirable on its own.
    Plain,
    /// `#[unique]`. At most one row, so the key alone identifies it and an
    /// insert asserts the key is free.
    Unique,
    /// Generated. One row per live record, keyed `(document, revision)`, which
    /// is what makes "is this revision still the current one" a question an
    /// assertion can ask of any record.
    Latest,
}

/// One index, either asked for by an attribute or written by the macro.
pub(crate) struct IndexSpec {
    /// The fields it is keyed on, in order. Empty for a generated index, whose
    /// key is not made of fields.
    fields: Vec<Ident>,
    types: Vec<Type>,
    kind: Kind,
}

impl IndexSpec {
    /// One index as an attribute asked for it, keyed on the fields it names.
    pub(crate) fn of(fields: Vec<Ident>, types: Vec<Type>, kind: Kind) -> Self {
        Self {
            fields,
            types,
            kind,
        }
    }

    /// The index every record gets, and the only one it is guaranteed to have.
    pub(crate) fn latest() -> Self {
        Self {
            fields: Vec::new(),
            types: Vec::new(),
            kind: Kind::Latest,
        }
    }

    /// The fields the key is made of, in key order.
    pub(crate) fn fields(&self) -> &[Ident] {
        &self.fields
    }

    /// What each of those fields is, in the same order.
    pub(crate) fn types(&self) -> &[Type] {
        &self.types
    }

    pub(crate) fn unique(&self) -> bool {
        self.kind == Kind::Unique
    }

    /// Whether this is the index the macro writes rather than one asked for.
    pub(crate) fn generated(&self) -> bool {
        self.kind == Kind::Latest
    }

    /// `user_by_country_city_state`, or `user_latest`.
    pub(crate) fn name(&self, record: &Ident) -> String {
        let mut name = snake(&record.to_string());
        if self.kind == Kind::Latest {
            name.push_str("_latest");
            return name;
        }
        name.push_str("_by");
        for field in &self.fields {
            name.push('_');
            name.push_str(&field.to_string());
        }
        name
    }

    /// `BY_COUNTRY_CITY_STATE`, or `LATEST`.
    pub(crate) fn constant(&self) -> Ident {
        let mut name = String::from(if self.kind == Kind::Latest {
            "LATEST"
        } else {
            "BY"
        });
        for field in &self.fields {
            name.push('_');
            name.push_str(&field.to_string().to_uppercase());
        }
        Ident::new(&name, Span::call_site())
    }

    /// How wide the key is.
    ///
    /// A plain index carries the document id as a trailing column, because
    /// `delete_index` retires a whole range and without it one record's row
    /// could not be retired without taking every other record that shares its
    /// key. The generated index is `(document, revision)`: the revision has to
    /// be in the key rather than the payload, because an assertion can only
    /// ever look at key bytes.
    pub(crate) fn arity(&self) -> usize {
        match self.kind {
            Kind::Unique => self.fields.len(),
            Kind::Plain => self.fields.len() + 1,
            Kind::Latest => 2,
        }
    }

    /// What each column of an index holds, so a store with a type system of its own
    /// can declare a real column instead of a blob.
    ///
    /// A field's type answers for itself through `ToOrdBytes`, which falls back to
    /// bytes for anything nobody has described. The trailing column of a non-unique
    /// index is the document id, and it answers the same way rather than being
    /// named here twice.
    pub(crate) fn column_types(&self) -> Vec<TokenStream> {
        let document = quote! {
            <::doclite::DocumentId as ::doclite::doclite_core::ToOrdBytes>::COLUMN_TYPE
        };
        if self.kind == Kind::Latest {
            return vec![
                document,
                quote! {
                    <::doclite::RevisionId as ::doclite::doclite_core::ToOrdBytes>::COLUMN_TYPE
                },
            ];
        }

        let mut columns: Vec<TokenStream> = self
            .types
            .iter()
            .map(|ty| quote!(<#ty as ::doclite::doclite_core::ToOrdBytes>::COLUMN_TYPE))
            .collect();
        if self.kind == Kind::Plain {
            columns.push(document);
        }
        columns
    }

    /// `key![self.country.clone(), self.city.clone(), document]`.
    ///
    /// A plain index ends in the document rather than the revision, so the key of a
    /// row does not move when the record is superseded and a foreign key written
    /// against it keeps finding the same rows.
    pub(crate) fn key_of(&self) -> TokenStream {
        if self.kind == Kind::Latest {
            return quote!(::doclite::doclite_core::key![document, revision]);
        }

        let fields = self.fields.iter().map(|field| quote!(self.#field.clone()));
        if self.kind == Kind::Plain {
            quote!(::doclite::doclite_core::key![#(#fields,)* document])
        } else {
            quote!(::doclite::doclite_core::key![#(#fields),*])
        }
    }
}

/// Which encoding a record's bytes are, chosen by `#[storage(..)]`.
///
/// A property of the record rather than of the store, so two records in one
/// database may disagree and nothing below the document layer is told either
/// way. ADR 0008.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    /// The default. Compact, and opaque to anything that is not this crate.
    Cbor,
    /// `#[storage(json)]`. Bigger on disk, bought for a store a person and a
    /// backend's own JSON functions can read.
    Json,
}

impl Codec {
    /// The macro that writes this codec's `ToStorage` and `FromStorage`.
    pub(crate) fn macro_name(self) -> Ident {
        match self {
            Self::Cbor => Ident::new("serde_storage", Span::call_site()),
            Self::Json => Ident::new("json_storage", Span::call_site()),
        }
    }
}

/// Everything a struct declares about how it is stored: the indexes it is filed
/// under, whether a revision of it may ever be superseded, and what its bytes
/// are encoded as.
pub(crate) struct Schema {
    indexes: Vec<IndexSpec>,
    amendable: bool,
    codec: Codec,
}

impl Schema {
    pub(crate) fn new(indexes: Vec<IndexSpec>, amendable: bool, codec: Codec) -> Self {
        Self {
            indexes,
            amendable,
            codec,
        }
    }

    pub(crate) fn indexes(&self) -> &[IndexSpec] {
        &self.indexes
    }

    /// Whether a revision of this record may ever be superseded.
    pub(crate) fn amendable(&self) -> bool {
        self.amendable
    }

    /// What this record's bytes are encoded as.
    pub(crate) fn codec(&self) -> Codec {
        self.codec
    }
}

/// `UserAccount` to `user_account`.
fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (at, character) in name.chars().enumerate() {
        if character.is_uppercase() {
            if at > 0 {
                out.push('_');
            }
            out.extend(character.to_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use syn::parse_quote;

    use super::*;

    fn record() -> Ident {
        parse_quote!(UserAccount)
    }

    fn spec(fields: &[&str], kind: Kind) -> IndexSpec {
        IndexSpec::of(
            fields
                .iter()
                .map(|name| Ident::new(name, Span::call_site()))
                .collect(),
            fields.iter().map(|_| parse_quote!(String)).collect(),
            kind,
        )
    }

    #[test]
    fn a_records_name_becomes_the_table_prefix() {
        assert_eq!(
            spec(&["country", "city"], Kind::Plain).name(&record()),
            "user_account_by_country_city"
        );
        assert_eq!(IndexSpec::latest().name(&record()), "user_account_latest");
    }

    #[test]
    fn a_constant_is_the_key_in_shouting_case() {
        assert_eq!(
            spec(&["country", "city"], Kind::Plain)
                .constant()
                .to_string(),
            "BY_COUNTRY_CITY"
        );
        assert_eq!(IndexSpec::latest().constant().to_string(), "LATEST");
    }

    /// ADR 0009's forced choice: a plain index carries the document id as a
    /// trailing column, or one record's row could not be retired without
    /// taking every other record that shares its key.
    #[test]
    fn only_a_plain_index_carries_the_document_id() {
        assert_eq!(spec(&["age"], Kind::Plain).arity(), 2);
        assert_eq!(spec(&["age"], Kind::Unique).arity(), 1);
        assert_eq!(IndexSpec::latest().arity(), 2);
    }

    #[test]
    fn every_column_of_a_key_says_what_holds_it() {
        assert_eq!(spec(&["age"], Kind::Plain).column_types().len(), 2);
        assert_eq!(spec(&["age"], Kind::Unique).column_types().len(), 1);
        assert_eq!(IndexSpec::latest().column_types().len(), 2);
    }

    /// The generated index is keyed `(document, revision)`; a plain one ends in
    /// the document, so a row's key does not move when the record is
    /// superseded.
    #[test]
    fn a_key_ends_in_what_its_kind_says_it_does() {
        assert!(spec(&["age"], Kind::Plain)
            .key_of()
            .to_string()
            .ends_with("document]"));
        assert!(spec(&["age"], Kind::Unique)
            .key_of()
            .to_string()
            .contains("self . age"));
        assert!(IndexSpec::latest()
            .key_of()
            .to_string()
            .contains("document , revision"));
    }

    #[test]
    fn a_camel_case_name_becomes_snake_case() {
        assert_eq!(snake("UserAccount"), "user_account");
        assert_eq!(snake("User"), "user");
        assert_eq!(snake("HTTPThing"), "h_t_t_p_thing");
    }
}
