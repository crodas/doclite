//! `#[derive(Column)]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Error, Fields, Result};

pub(crate) fn expand(input: &DeriveInput) -> Result<TokenStream> {
    let name = &input.ident;
    let (generics, types, where_clause) = input.generics.split_for_impl();

    let (body, column_type) = match &input.data {
        Data::Struct(data) => (
            newtype(input, &data.fields)?,
            wrapped_type(input, &data.fields)?,
        ),
        Data::Enum(data) => (
            fieldless_enum(input, data)?,
            quote!(::doclite::doclite_core::ColumnType::Str),
        ),
        Data::Union(_) => {
            return Err(Error::new_spanned(
                input,
                "a union has no value to encode; Column wants a newtype or a fieldless enum",
            ))
        }
    };

    Ok(quote! {
        impl #generics ::doclite::doclite_core::ToOrdBytes for #name #types #where_clause {
            const COLUMN_TYPE: ::doclite::doclite_core::ColumnType = #column_type;

            fn to_ord_bytes(&self) -> ::std::vec::Vec<u8> {
                #body
            }
        }

        impl #generics ::doclite::doclite_core::IntoColumn for #name #types #where_clause {
            fn into_column(self) -> ::doclite::doclite_core::Column {
                ::doclite::doclite_core::Column::encode(&self)
            }
        }
    })
}

/// A newtype is what it wraps, so a store that lays out columns by type sees
/// through it rather than reaching for the byte fallback.
fn wrapped_type(input: &DeriveInput, fields: &Fields) -> Result<TokenStream> {
    let inner = match fields {
        Fields::Unnamed(unnamed) if unnamed.unnamed.len() == 1 => {
            unnamed.unnamed.first().map(|field| field.ty.clone())
        }
        Fields::Named(named) if named.named.len() == 1 => {
            named.named.first().map(|field| field.ty.clone())
        }
        _ => None,
    };
    match inner {
        Some(ty) => Ok(quote!(<#ty as ::doclite::doclite_core::ToOrdBytes>::COLUMN_TYPE)),
        None => Err(Error::new_spanned(
            input,
            "Column wants a struct of exactly one field, so there is one value to encode",
        )),
    }
}

/// A newtype is the value it wraps, so its ordering is that value's ordering.
fn newtype(input: &DeriveInput, fields: &Fields) -> Result<TokenStream> {
    let field = match fields {
        Fields::Unnamed(unnamed) if unnamed.unnamed.len() == 1 => quote!(self.0),
        Fields::Named(named) if named.named.len() == 1 => {
            let name = named.named.first().and_then(|field| field.ident.as_ref());
            quote!(self.#name)
        }
        _ => {
            return Err(Error::new_spanned(
                input,
                "Column wants a struct of exactly one field, so there is one value to encode",
            ))
        }
    };
    Ok(quote!(::doclite::doclite_core::ToOrdBytes::to_ord_bytes(&#field)))
}

/// A variant encodes as its own name.
///
/// The alternative is its position, which sorts by declaration order and would
/// silently change what every index row written so far means the first time
/// somebody reorders the enum. A rename breaks compilation instead.
fn fieldless_enum(input: &DeriveInput, data: &syn::DataEnum) -> Result<TokenStream> {
    let arms = data
        .variants
        .iter()
        .map(|variant| {
            if !matches!(variant.fields, Fields::Unit) {
                return Err(Error::new_spanned(
                    variant,
                    "Column wants a fieldless enum; a variant carrying data has no single ordering",
                ));
            }
            let ident = &variant.ident;
            let text = ident.to_string();
            Ok(quote!(Self::#ident => #text.as_bytes().to_vec()))
        })
        .collect::<Result<Vec<_>>>()?;

    if arms.is_empty() {
        return Err(Error::new_spanned(
            input,
            "an enum with no variants has no value to encode",
        ));
    }

    Ok(quote!(match self { #(#arms),* }))
}
