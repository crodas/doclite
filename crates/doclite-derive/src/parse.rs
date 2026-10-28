//! Reading off a struct's attributes what it declares about being stored.

use syn::{
    Data, DeriveInput, Error, Fields, GenericArgument, Ident, Meta, PathArguments, Result, Type,
};

use crate::spec::{Codec, IndexSpec, Kind, Schema};

pub(crate) fn schema(input: &DeriveInput) -> Result<Schema> {
    Ok(Schema::new(
        indexes(input)?,
        !immutable(input)?,
        codec(input)?,
    ))
}

/// Which encoding the struct asked its bytes to be, defaulting to CBOR.
///
/// Refused on a field and refused without an argument, for the reason
/// `immutable` refuses the same: a record stored in a format its author did not
/// mean is not something to find out from a hex dump.
///
/// `json` is refused on a record carrying bytes, because `serde_json` writes a
/// `Vec<u8>` as an array of numbers. The check is syntactic, so a
/// `type Blob = Vec<u8>` alias or a `struct Png(Vec<u8>)` newtype slips past it;
/// it catches the case that gets written rather than every case that exists.
fn codec(input: &DeriveInput) -> Result<Codec> {
    if let Data::Struct(data) = &input.data {
        for field in &data.fields {
            if let Some(attribute) = field
                .attrs
                .iter()
                .find(|attribute| attribute.path().is_ident("storage"))
            {
                return Err(Error::new_spanned(
                    attribute,
                    "storage belongs on the struct; it is the whole record that is encoded",
                ));
            }
        }
    }

    let mut chosen = Codec::Cbor;
    for attribute in &input.attrs {
        if !attribute.path().is_ident("storage") {
            continue;
        }
        let Meta::List(list) = &attribute.meta else {
            return Err(Error::new_spanned(
                attribute,
                "storage takes one encoding: storage(json) or storage(cbor)",
            ));
        };
        let named: Ident = list.parse_args().map_err(|_| {
            Error::new_spanned(
                attribute,
                "storage takes one encoding: storage(json) or storage(cbor)",
            )
        })?;
        chosen = match named.to_string().as_str() {
            "cbor" => Codec::Cbor,
            "json" => Codec::Json,
            other => {
                return Err(Error::new_spanned(
                    &named,
                    format!("no encoding named {other}; storage takes json or cbor"),
                ))
            }
        };
    }

    if chosen == Codec::Json {
        refuse_bytes(input)?;
    }
    Ok(chosen)
}

/// The first field JSON would turn into an array of numbers, refused by name.
fn refuse_bytes(input: &DeriveInput) -> Result<()> {
    let Data::Struct(data) = &input.data else {
        return Ok(());
    };
    for field in &data.fields {
        if !holds_bytes(&field.ty) {
            continue;
        }
        let named = field
            .ident
            .as_ref()
            .map(|ident| format!("`{ident}`"))
            .unwrap_or_else(|| "this field".to_owned());
        return Err(Error::new_spanned(
            field,
            format!(
                "{named} cannot be stored as JSON; bytes would encode as an array of numbers. \
                 Drop storage(json) to store this record as CBOR"
            ),
        ));
    }
    Ok(())
}

/// The owner a `Vec<T>` holds several of, and `None` for a field that is one
/// owner.
///
/// Read off the written type, the way [`holds_bytes`] is: a `type Owners =
/// Vec<Ref<User>>` alias goes through as one owner and fails to compile at the
/// column it cannot be. It catches what gets written rather than everything
/// that could be.
fn element(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    let last = path.path.segments.last()?;
    if last.ident != "Vec" {
        return None;
    }
    let PathArguments::AngleBracketed(arguments) = &last.arguments else {
        return None;
    };
    let Some(GenericArgument::Type(inner)) = arguments.args.first() else {
        return None;
    };
    Some(inner)
}

/// Whether a type is one JSON would write as an array of numbers.
///
/// Looks through `Option` and a reference, which is where a byte field actually
/// turns up, and no further.
fn holds_bytes(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => holds_bytes(&reference.elem),
        Type::Slice(slice) => is_u8(&slice.elem),
        Type::Array(array) => is_u8(&array.elem),
        Type::Path(path) => {
            let Some(last) = path.path.segments.last() else {
                return false;
            };
            let PathArguments::AngleBracketed(arguments) = &last.arguments else {
                return false;
            };
            let Some(GenericArgument::Type(inner)) = arguments.args.first() else {
                return false;
            };
            match last.ident.to_string().as_str() {
                "Vec" => is_u8(inner),
                "Option" | "Box" | "Cow" => holds_bytes(inner),
                _ => false,
            }
        }
        _ => false,
    }
}

fn is_u8(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if path.path.is_ident("u8"))
}

/// Whether the struct said it has exactly one revision, ever.
///
/// The attribute takes no arguments and belongs to the struct rather than a
/// field, and both are refused rather than ignored: a record that meant to be
/// immutable and is quietly not one is the failure this whole attribute exists
/// to prevent.
fn immutable(input: &DeriveInput) -> Result<bool> {
    if let Data::Struct(data) = &input.data {
        for field in &data.fields {
            if let Some(attribute) = field
                .attrs
                .iter()
                .find(|attribute| attribute.path().is_ident("immutable"))
            {
                return Err(Error::new_spanned(
                    attribute,
                    "immutable belongs on the struct; it is the whole record that is never superseded",
                ));
            }
        }
    }

    let mut marked = false;
    for attribute in &input.attrs {
        if !attribute.path().is_ident("immutable") {
            continue;
        }
        if !matches!(attribute.meta, Meta::Path(_)) {
            return Err(Error::new_spanned(
                attribute,
                "immutable takes no arguments; it says the record is never superseded",
            ));
        }
        marked = true;
    }
    Ok(marked)
}

/// Whether the struct put `#[owner]` where it cannot go.
///
/// An owner is one field's value, so the attribute belongs on that field. An
/// owner made of several fields is a feed keyed on a tuple, which nothing here
/// writes yet, and reading `#[owner(clinic, patient)]` as the first component
/// alone would file every change under the wrong key.
fn owner_on_the_struct(input: &DeriveInput) -> Result<()> {
    for attribute in &input.attrs {
        if attribute.path().is_ident("owner") {
            return Err(Error::new_spanned(
                attribute,
                "owner belongs on the field that holds it; an owner made of several fields \
                 is not something this writes",
            ));
        }
    }
    Ok(())
}

fn indexes(input: &DeriveInput) -> Result<Vec<IndexSpec>> {
    owner_on_the_struct(input)?;

    let Data::Struct(data) = &input.data else {
        return Err(Error::new_spanned(
            input,
            "Document wants a struct; there is nothing to index on an enum or a union",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(Error::new_spanned(
            input,
            "Document wants named fields, because an index names the fields it is keyed on",
        ));
    };

    let mut specs = Vec::new();
    let mut owner: Option<Ident> = None;

    for field in &named.named {
        let Some(ident) = field.ident.clone() else {
            continue;
        };
        for attribute in &field.attrs {
            if attribute.path().is_ident("unique") {
                specs.push(IndexSpec::of(
                    vec![ident.clone()],
                    vec![field.ty.clone()],
                    Kind::Unique,
                ));
            } else if attribute.path().is_ident("index") {
                specs.push(IndexSpec::of(
                    vec![ident.clone()],
                    vec![field.ty.clone()],
                    Kind::Plain,
                ));
            } else if attribute.path().is_ident("owner") {
                if !matches!(attribute.meta, Meta::Path(_)) {
                    return Err(Error::new_spanned(
                        attribute,
                        "owner takes no arguments; the field it is on is the owner",
                    ));
                }
                if let Some(first) = &owner {
                    return Err(Error::new_spanned(
                        &ident,
                        format!(
                            "`{first}` is already this record's owner; a record has one feed, \
                             and which owner a change belongs to cannot be two answers"
                        ),
                    ));
                }
                owner = Some(ident.clone());
                let (ty, many) = match element(&field.ty) {
                    Some(element) => (element.clone(), true),
                    None => (field.ty.clone(), false),
                };
                specs.push(IndexSpec::owner(ident.clone(), ty, many));
            }
        }
    }

    for attribute in &input.attrs {
        if !attribute.path().is_ident("index") {
            continue;
        }
        let mut fields = Vec::new();
        attribute.parse_nested_meta(|meta| {
            let ident = meta
                .path
                .get_ident()
                .cloned()
                .ok_or_else(|| meta.error("an index names a field of this struct"))?;
            fields.push(ident);
            Ok(())
        })?;

        let types = fields
            .iter()
            .map(|wanted| {
                named
                    .named
                    .iter()
                    .find(|field| field.ident.as_ref() == Some(wanted))
                    .map(|field| field.ty.clone())
                    .ok_or_else(|| {
                        Error::new_spanned(wanted, format!("no field named `{wanted}` to index"))
                    })
            })
            .collect::<Result<Vec<_>>>()?;

        if fields.is_empty() {
            return Err(Error::new_spanned(
                attribute,
                "an index over no fields selects everything, which is what omitting it does",
            ));
        }

        specs.push(IndexSpec::of(fields, types, Kind::Plain));
    }

    if specs.is_empty() {
        return Err(Error::new_spanned(
            input,
            "a record needs at least one index, or nothing can find it again",
        ));
    }
    if let Some(twice) = repeated(&specs) {
        return Err(twice);
    }

    specs.insert(0, IndexSpec::latest());
    Ok(specs)
}

/// Two indexes keyed on the same fields are one index asked for twice.
///
/// `#[unique] age` beside `#[index(age)]` is the case. Both name a `BY_AGE`
/// constant and a `user_by_age` table, at two different widths, so leaving it
/// alone gives the caller a duplicate-definition error spanned at
/// `#[derive(Document)]` rather than at the attribute they wrote. It is refused
/// here because here is where the span is.
///
/// A feed is exempt, because it names `FEED` and `user_feed` whatever field it
/// is keyed on: `#[owner]` and `#[index]` on one field are two indexes of that
/// field at two shapes, which is the ordinary way to both scope and feed it.
fn repeated(specs: &[IndexSpec]) -> Option<Error> {
    let mut held: Vec<&[Ident]> = Vec::new();
    for spec in specs.iter().filter(|spec| !spec.feed()) {
        let fields = spec.fields();
        if held.iter().any(|seen| same(seen, fields)) {
            let named = fields
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Some(Error::new_spanned(
                fields.first()?,
                format!("({named}) is already an index of this record; one key is one index, however many attributes ask for it"),
            ));
        }
        held.push(fields);
    }
    None
}

fn same(left: &[Ident], right: &[Ident]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(one, two)| one == two)
}

#[cfg(test)]
mod tests {
    use syn::parse_quote;

    use super::*;

    fn refusal(input: DeriveInput) -> String {
        match schema(&input) {
            Ok(_) => panic!("this should not have parsed"),
            Err(why) => why.to_string(),
        }
    }

    fn chosen(input: DeriveInput) -> Codec {
        schema(&input).expect("this parses").codec()
    }

    fn kinds(input: DeriveInput) -> Vec<String> {
        schema(&input)
            .expect("this parses")
            .indexes()
            .iter()
            .map(|spec| spec.constant().to_string())
            .collect()
    }

    /// One field can be both scoped and fed: the two indexes are keyed
    /// differently and neither name collides with the other.
    #[test]
    fn a_field_can_be_both_an_index_and_the_owner() {
        assert_eq!(
            kinds(parse_quote! {
                struct Deal {
                    #[owner]
                    #[index]
                    account: String,
                }
            }),
            ["LATEST", "FEED", "BY_ACCOUNT"],
            "the generated index, then the attributes in the order they are written"
        );
    }

    /// A `Vec` owner keeps the element as the column type, because a key holds
    /// one owner whichever shape the field has.
    #[test]
    fn a_vec_of_owners_is_keyed_on_one_of_them() {
        let declared = schema(&parse_quote! {
            struct Deal {
                #[owner]
                watchers: Vec<String>,
            }
        })
        .expect("this parses");

        let feed = declared
            .indexes()
            .iter()
            .find(|spec| spec.feed())
            .expect("the feed is declared");

        assert_eq!(feed.arity(), 3);
        assert_eq!(
            feed.column_types()[0].to_string(),
            quote::quote!(<String as ::doclite::doclite_core::ToOrdBytes>::COLUMN_TYPE).to_string(),
            "the element answers for the column, not the list"
        );
        assert!(feed
            .files(&parse_quote!(Deal))
            .to_string()
            .contains("for owner"));
    }

    #[test]
    fn a_record_with_a_second_owner_is_refused() {
        let why = refusal(parse_quote! {
            struct Deal {
                #[owner]
                account: String,
                #[owner]
                clinic: String,
            }
        });
        assert!(
            why.contains("`account` is already this record's owner"),
            "{why}"
        );
    }

    #[test]
    fn an_owner_on_the_struct_is_refused() {
        let why = refusal(parse_quote! {
            #[owner(clinic, patient)]
            struct Visit {
                #[unique]
                reference: String,
            }
        });
        assert!(why.contains("belongs on the field"), "{why}");
    }

    #[test]
    fn an_owner_taking_arguments_is_refused() {
        let why = refusal(parse_quote! {
            struct Deal {
                #[owner(account)]
                account: String,
            }
        });
        assert!(why.contains("takes no arguments"), "{why}");
    }

    #[test]
    fn a_record_that_says_nothing_is_cbor() {
        assert!(
            chosen(parse_quote! {
                struct User {
                    #[unique]
                    email: String,
                }
            }) == Codec::Cbor
        );
    }

    #[test]
    fn a_record_can_ask_for_either_encoding() {
        assert!(
            chosen(parse_quote! {
                #[storage(json)]
                struct User {
                    #[unique]
                    email: String,
                }
            }) == Codec::Json
        );
        assert!(
            chosen(parse_quote! {
                #[storage(cbor)]
                struct User {
                    #[unique]
                    email: String,
                }
            }) == Codec::Cbor
        );
    }

    /// The refusal the check exists for: JSON would write these bytes as an
    /// array of numbers, which round-trips and is unreadable, so the record is
    /// told to stay CBOR rather than quietly stored that way.
    #[test]
    fn json_on_a_record_carrying_bytes_is_refused_by_field_name() {
        let why = refusal(parse_quote! {
            #[storage(json)]
            struct Scan {
                #[unique]
                reference: String,
                image: Vec<u8>,
            }
        });
        assert!(why.contains("`image`"), "it names the field: {why}");
        assert!(why.contains("array of numbers"), "and says why: {why}");
    }

    /// Where a byte field actually turns up, rather than only the bare `Vec`.
    #[test]
    fn bytes_are_found_through_an_option_and_behind_an_array() {
        for input in [
            parse_quote! {
                #[storage(json)]
                struct Scan {
                    #[unique]
                    reference: String,
                    image: Option<Vec<u8>>,
                }
            },
            parse_quote! {
                #[storage(json)]
                struct Scan {
                    #[unique]
                    reference: String,
                    digest: [u8; 32],
                }
            },
        ] {
            let why = refusal(input);
            assert!(why.contains("array of numbers"), "{why}");
        }
    }

    /// A record of ordinary fields is not caught by the byte check, which would
    /// otherwise be a refusal nobody could satisfy.
    #[test]
    fn a_record_of_scalars_is_left_alone() {
        assert!(
            chosen(parse_quote! {
                #[storage(json)]
                struct User {
                    #[unique]
                    email: String,
                    ages: Vec<u32>,
                    nickname: Option<String>,
                }
            }) == Codec::Json
        );
    }

    #[test]
    fn storage_on_a_field_is_refused() {
        let why = refusal(parse_quote! {
            struct User {
                #[storage(json)]
                #[unique]
                email: String,
            }
        });
        assert!(why.contains("belongs on the struct"), "{why}");
    }

    #[test]
    fn storage_without_an_encoding_is_refused() {
        let why = refusal(parse_quote! {
            #[storage]
            struct User {
                #[unique]
                email: String,
            }
        });
        assert!(why.contains("storage(json) or storage(cbor)"), "{why}");
    }

    #[test]
    fn an_encoding_nobody_implements_is_named_rather_than_ignored() {
        let why = refusal(parse_quote! {
            #[storage(yaml)]
            struct User {
                #[unique]
                email: String,
            }
        });
        assert!(why.contains("no encoding named yaml"), "{why}");
    }

    fn widths(input: DeriveInput) -> Vec<usize> {
        schema(&input)
            .expect("this parses")
            .indexes()
            .iter()
            .map(IndexSpec::arity)
            .collect()
    }

    #[test]
    fn a_record_gets_the_generated_index_and_the_ones_it_asked_for() {
        let widths = widths(parse_quote! {
            #[index(country, city)]
            struct User {
                #[unique]
                email: String,
                country: String,
                city: String,
            }
        });
        assert_eq!(widths, vec![2, 1, 3], "latest, then unique, then compound");
    }

    /// The failure the whole attribute exists to prevent is a record that meant
    /// to be immutable and quietly is not, so a misplaced one is refused.
    #[test]
    fn immutable_on_a_field_is_refused() {
        let why = refusal(parse_quote! {
            struct User {
                #[immutable]
                #[unique]
                email: String,
            }
        });
        assert!(why.contains("belongs on the struct"), "{why}");
    }

    #[test]
    fn immutable_with_arguments_is_refused() {
        let why = refusal(parse_quote! {
            #[immutable(yes)]
            struct User {
                #[unique]
                email: String,
            }
        });
        assert!(why.contains("takes no arguments"), "{why}");
    }

    #[test]
    fn an_enum_has_nothing_to_index() {
        let why = refusal(parse_quote! {
            enum User {
                One,
            }
        });
        assert!(why.contains("wants a struct"), "{why}");
    }

    #[test]
    fn a_tuple_struct_has_no_field_names_to_key_on() {
        let why = refusal(parse_quote!(
            struct User(String);
        ));
        assert!(why.contains("named fields"), "{why}");
    }

    #[test]
    fn an_index_over_a_field_that_is_not_there_names_it() {
        let why = refusal(parse_quote! {
            #[index(nowhere)]
            struct User {
                #[unique]
                email: String,
            }
        });
        assert!(why.contains("no field named `nowhere`"), "{why}");
    }

    #[test]
    fn an_index_over_no_fields_is_refused() {
        let why = refusal(parse_quote! {
            #[index()]
            struct User {
                #[unique]
                email: String,
            }
        });
        assert!(why.contains("selects everything"), "{why}");
    }

    #[test]
    fn a_record_with_no_index_at_all_is_refused() {
        let why = refusal(parse_quote! {
            struct User {
                email: String,
            }
        });
        assert!(why.contains("at least one index"), "{why}");
    }

    /// Two attributes asking for one key used to emit two `BY_AGE` constants
    /// and a duplicate-definition error pointing at the derive.
    #[test]
    fn one_key_asked_for_twice_is_refused_where_it_was_asked() {
        let why = refusal(parse_quote! {
            #[index(age)]
            struct User {
                #[unique]
                email: String,
                #[index]
                age: u8,
            }
        });
        assert!(why.contains("(age) is already an index"), "{why}");
    }

    /// Two compound indexes over the same fields in a different order are two
    /// indexes: the key order is the scan order.
    #[test]
    fn the_same_fields_in_another_order_are_another_index() {
        let widths = widths(parse_quote! {
            #[index(country, city)]
            #[index(city, country)]
            struct User {
                country: String,
                city: String,
            }
        });
        assert_eq!(widths, vec![2, 3, 3]);
    }
}
