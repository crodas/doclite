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
    /// `#[owner]`. One row per revision rather than per record, keyed
    /// `(owner, revision, document)`, so an owner's changes read in the order
    /// they happened and a row is never retired.
    Feed,
}

/// One index, either asked for by an attribute or written by the macro.
pub(crate) struct IndexSpec {
    /// The fields it is keyed on, in order. Empty for a generated index, whose
    /// key is not made of fields.
    fields: Vec<Ident>,
    types: Vec<Type>,
    kind: Kind,
    /// Whether the field holds several owners rather than one, which only a
    /// feed can be.
    many: bool,
}

impl IndexSpec {
    /// One index as an attribute asked for it, keyed on the fields it names.
    pub(crate) fn of(fields: Vec<Ident>, types: Vec<Type>, kind: Kind) -> Self {
        Self {
            fields,
            types,
            kind,
            many: false,
        }
    }

    /// The feed one `#[owner]` field declares.
    ///
    /// `ty` is what one owner is, which for a field holding several is the
    /// element rather than the list: the key holds one owner, so a column
    /// holds one owner, and a feed is read for one owner whichever shape the
    /// field has.
    pub(crate) fn owner(field: Ident, ty: Type, many: bool) -> Self {
        Self {
            fields: vec![field],
            types: vec![ty],
            kind: Kind::Feed,
            many,
        }
    }

    /// The index every record gets, and the only one it is guaranteed to have.
    pub(crate) fn latest() -> Self {
        Self {
            fields: Vec::new(),
            types: Vec::new(),
            kind: Kind::Latest,
            many: false,
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

    /// Which `Filing` a walk is handed for this index, which is what decides
    /// whether its row is claimed, and whether it is retired.
    pub(crate) fn filing(&self) -> TokenStream {
        match self.kind {
            Kind::Latest => quote!(::doclite::Filing::Latest),
            Kind::Plain => quote!(::doclite::Filing::Plain),
            Kind::Unique => quote!(::doclite::Filing::Unique),
            Kind::Feed => quote!(::doclite::Filing::Feed),
        }
    }

    /// Whether this is the index the macro writes rather than one asked for.
    pub(crate) fn generated(&self) -> bool {
        self.kind == Kind::Latest
    }

    /// Whether this index accumulates a row per revision rather than holding
    /// one per record.
    pub(crate) fn feed(&self) -> bool {
        self.kind == Kind::Feed
    }

    /// `user_by_country_city_state`, `user_latest`, or `user_feed`.
    pub(crate) fn name(&self, record: &Ident) -> String {
        let mut name = snake(&record.to_string());
        if self.kind == Kind::Latest {
            name.push_str("_latest");
            return name;
        }
        if self.kind == Kind::Feed {
            name.push_str("_feed");
            return name;
        }
        name.push_str("_by");
        for field in &self.fields {
            name.push('_');
            name.push_str(&field.to_string());
        }
        name
    }

    /// `BY_COUNTRY_CITY_STATE`, `LATEST`, or `FEED`.
    pub(crate) fn constant(&self) -> Ident {
        if self.kind == Kind::Feed {
            return Ident::new("FEED", Span::call_site());
        }
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
    /// ever look at key bytes. A feed ends in both, the revision first: that
    /// is what orders an owner's rows by when they happened, and the document
    /// after it keeps two records revised in one millisecond apart.
    pub(crate) fn arity(&self) -> usize {
        match self.kind {
            Kind::Unique => self.fields.len(),
            Kind::Plain => self.fields.len() + 1,
            Kind::Latest => 2,
            Kind::Feed => self.fields.len() + 2,
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
        if self.kind == Kind::Feed {
            columns.push(quote! {
                <::doclite::RevisionId as ::doclite::doclite_core::ToOrdBytes>::COLUMN_TYPE
            });
        }
        if self.kind == Kind::Plain || self.kind == Kind::Feed {
            columns.push(document);
        }
        columns
    }

    /// The statement a walk runs for this index.
    ///
    /// One call per row, which for a field holding several owners is one call
    /// each: the index does not change shape, a revision just lands in every
    /// owner's feed. An empty list files nothing, which is what a record with
    /// no owners means.
    pub(crate) fn files(&self, record: &Ident) -> TokenStream {
        let constant = self.constant();
        let name = self.name(record);
        let filing = self.filing();

        if !self.many {
            let key = self.key_of();
            return quote! {
                into.row(&#record::#constant, #key, #name, #filing);
            };
        }

        let field = &self.fields[0];
        quote! {
            for owner in &self.#field {
                into.row(
                    &#record::#constant,
                    ::doclite::doclite_core::key![owner.clone(), revision, document],
                    #name,
                    #filing,
                );
            }
        }
    }

    /// `key![self.country.clone(), self.city.clone(), document]`.
    ///
    /// A plain index ends in the document rather than the revision, so the key of a
    /// row does not move when the record is superseded and a foreign key written
    /// against it keeps finding the same rows. A feed ends in both, because its
    /// rows are what happened rather than what is current.
    pub(crate) fn key_of(&self) -> TokenStream {
        if self.kind == Kind::Latest {
            return quote!(::doclite::doclite_core::key![document, revision]);
        }

        let fields = self.fields.iter().map(|field| quote!(self.#field.clone()));
        match self.kind {
            Kind::Plain => quote!(::doclite::doclite_core::key![#(#fields,)* document]),
            Kind::Feed => {
                quote!(::doclite::doclite_core::key![#(#fields,)* revision, document])
            }
            Kind::Unique | Kind::Latest => quote!(::doclite::doclite_core::key![#(#fields),*]),
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

    fn owner(field: &str, ty: Type, many: bool) -> IndexSpec {
        IndexSpec::owner(Ident::new(field, Span::call_site()), ty, many)
    }

    /// Several owners do not change the index. One revision lands in each of
    /// their feeds, which is one call per owner rather than a wider key.
    #[test]
    fn a_field_holding_several_owners_files_one_row_each() {
        let many = owner("watchers", parse_quote!(String), true);
        assert_eq!(many.arity(), 3, "the key still holds one owner");

        let files = many.files(&record()).to_string();
        assert!(files.contains("for owner in & self . watchers"), "{files}");
        assert!(files.contains("owner . clone ()"), "{files}");
        assert!(files.contains("revision , document"), "{files}");
    }

    /// One owner is the call the other indexes get, and not a loop over
    /// something that is not a list.
    #[test]
    fn a_field_holding_one_owner_files_one_row() {
        let one = owner("account", parse_quote!(String), false);
        let files = one.files(&record()).to_string();
        assert!(!files.contains("for owner"), "{files}");
        assert!(files.contains("self . account . clone ()"), "{files}");
    }

    /// A feed is named for what it is rather than for the field it is keyed on,
    /// so a record has one however its owner is spelled.
    #[test]
    fn a_feed_is_named_after_the_record_and_nothing_else() {
        let feed = spec(&["account"], Kind::Feed);
        assert_eq!(feed.name(&record()), "user_account_feed");
        assert_eq!(feed.constant().to_string(), "FEED");
    }

    /// The revision comes before the document, because that is what puts an
    /// owner's rows in the order the changes happened.
    #[test]
    fn a_feed_key_ends_in_the_revision_then_the_document() {
        let feed = spec(&["account"], Kind::Feed);
        assert_eq!(feed.arity(), 3);
        assert_eq!(feed.column_types().len(), 3);
        let key = feed.key_of().to_string();
        assert!(key.contains("self . account"), "{key}");
        assert!(key.ends_with("revision , document]"), "{key}");
    }

    /// Only the generated index answers `Record::latest`, so a feed must not
    /// be mistaken for it.
    #[test]
    fn a_feed_is_not_the_generated_index() {
        let feed = spec(&["account"], Kind::Feed);
        assert!(feed.feed());
        assert!(!feed.generated());
        assert!(!IndexSpec::latest().feed());
        assert_eq!(feed.filing().to_string(), ":: doclite :: Filing :: Feed");
    }

    #[test]
    fn a_camel_case_name_becomes_snake_case() {
        assert_eq!(snake("UserAccount"), "user_account");
        assert_eq!(snake("User"), "user");
        assert_eq!(snake("HTTPThing"), "h_t_t_p_thing");
    }
}
