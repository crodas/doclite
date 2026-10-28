//! `#[derive(Document)]`.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{DeriveInput, Error, Ident, Result};

use crate::parse::schema;
use crate::spec::IndexSpec;

pub(crate) fn expand(input: &DeriveInput) -> Result<TokenStream> {
    let record = &input.ident;
    let declared = schema(input)?;
    let specs = declared.indexes();

    let constants = specs.iter().map(|spec| {
        let constant = spec.constant();
        let name = spec.name(record);
        let arity = spec.arity();
        let columns = spec.column_types();
        let doc = format!("The `{name}` index, as this record declares it.");
        quote! {
            #[doc = #doc]
            pub const #constant: ::doclite::doclite_core::Index<#arity> =
                ::doclite::doclite_core::Index::typed(#name, &[#(#columns),*]);
        }
    });

    let schemas = specs.iter().map(|spec| {
        let constant = spec.constant();
        quote!(#record::#constant.schema())
    });

    let latest = specs
        .iter()
        .find(|spec| spec.generated())
        .map(IndexSpec::constant)
        .ok_or_else(|| Error::new_spanned(input, "every record is given a latest index"))?;

    let feeds = specs.iter().find(|spec| spec.feed()).map_or_else(
        || quote!(::std::option::Option::None),
        |spec| {
            let constant = spec.constant();
            quote!(::std::option::Option::Some(#record::#constant.id()))
        },
    );

    let rows = specs.iter().map(|spec| spec.files(record));

    let feed = specs.iter().find(|spec| spec.feed()).map(|spec| {
        let constant = spec.constant();
        let owner = &spec.fields()[0];
        let ty = &spec.types()[0];
        let doc = format!(
            "Reads `{}` for one owner: a row per revision, oldest first.",
            spec.name(record)
        );
        quote! {
            #[doc = #doc]
            pub fn feed<E, C>(
                conn: &::doclite::Connection<E, C>,
                #owner: #ty,
            ) -> ::doclite::Feed<'_, E, C, #record>
            where
                E: ::doclite::doclite_core::Engine,
                C: ::doclite::Clock + ::std::marker::Copy,
            {
                ::doclite::Feed::new(
                    conn,
                    #record::#constant.id(),
                    ::doclite::doclite_core::IntoColumn::into_column(#owner),
                    |spec| ::doclite::feed::scanning(&#record::#constant, spec),
                )
            }
        }
    });

    let builder = format_ident!("{record}Query");
    let methods = specs
        .iter()
        .filter(|spec| !spec.feed())
        .flat_map(|spec| by_methods(record, spec))
        .collect::<Vec<_>>();

    let amendable = declared.amendable().then(|| {
        quote! {
            impl ::doclite::Amendable for #record {}
        }
    });

    let storage = declared.codec().macro_name();

    Ok(quote! {
        ::doclite::#storage!(#record);

        #amendable

        impl #record {
            #(#constants)*

            /// Starts a query against this record's indexes.
            pub fn query<E, C>(conn: &::doclite::Connection<E, C>) -> #builder<'_, E, C>
            where
                E: ::doclite::doclite_core::Engine,
                C: ::doclite::Clock + ::std::marker::Copy,
            {
                #builder(::doclite::Selection::new(conn))
            }

            #feed
        }

        impl ::doclite::Record for #record {
            const FEEDS: ::std::option::Option<::doclite::doclite_core::IndexId> = #feeds;

            fn schemas() -> ::std::vec::Vec<::doclite::doclite_core::IndexSchema> {
                ::std::vec![#(#schemas),*]
            }

            fn latest() -> ::doclite::doclite_core::Index<2usize> {
                #record::#latest
            }

            fn rows<R: ::doclite::Rows>(
                &self,
                document: ::doclite::DocumentId,
                revision: ::doclite::RevisionId,
                into: &mut R,
            ) {
                #(#rows)*
            }
        }

        /// A query against one of this record's indexes.
        pub struct #builder<'a, E, C>(::doclite::Selection<'a, E, C, #record>);

        impl<'a, E, C> #builder<'a, E, C>
        where
            E: ::doclite::doclite_core::Engine,
            C: ::doclite::Clock + ::std::marker::Copy,
        {
            #(#methods)*

            /// Rows to return at most.
            pub fn limit(self, rows: usize) -> Self {
                Self(self.0.limit(rows))
            }

            /// Every match, up to the limit.
            pub async fn get_many(
                self,
            ) -> ::std::result::Result<
                ::std::vec::Vec<::doclite::Stored<#record>>,
                ::doclite::DocumentError,
            > {
                self.0.get_many().await
            }

            /// The one match, refusing rather than choosing between several.
            pub async fn get_one(
                self,
            ) -> ::std::result::Result<
                ::doclite::Stored<#record>,
                ::doclite::DocumentError,
            > {
                self.0.get_one().await
            }
        }
    })
}

/// One `by_` method per prefix of the index, since a prefix is what an ordered
/// index can answer. The earlier components are equalities and only the last
/// takes bounds.
///
/// A feed gets none of these. Its rows are read for one owner and ranged on the
/// revision, which is not a prefix of the fields it names, and a `by_account`
/// here would collide with the one `#[index] account` writes.
fn by_methods(record: &Ident, spec: &IndexSpec) -> Vec<TokenStream> {
    (1..=spec.fields().len())
        .map(|width| by_method(record, spec, width))
        .collect()
}

fn by_method(record: &Ident, spec: &IndexSpec, width: usize) -> TokenStream {
    let named = &spec.fields()[..width];
    let method = Ident::new(
        &format!(
            "by_{}",
            named
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("_")
        ),
        Span::call_site(),
    );

    let constant = spec.constant();
    let arity = spec.arity();
    let last = width - 1;

    let ranged = &named[last];
    let doc = if last == 0 {
        format!("Reads `{}`, ranging on `{ranged}`.", spec.name(record))
    } else {
        let pinned = named[..last]
            .iter()
            .map(|field| format!("`{field}`"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "Reads `{}`, pinning {pinned} and ranging on `{ranged}`.",
            spec.name(record)
        )
    };

    let leading: Vec<Ident> = named[..last].to_vec();
    let leading_types = &spec.types()[..last];
    let parameters = leading
        .iter()
        .zip(leading_types)
        .map(|(name, ty)| quote!(#name: #ty));

    let bound_type = &spec.types()[last];
    let pinned = leading
        .iter()
        .map(|name| quote!(::doclite::doclite_core::IntoColumn::into_column(#name)));
    let pinned: Vec<TokenStream> = pinned.collect();
    let cloned: Vec<TokenStream> = (0..last)
        .map(|at| {
            let name = format_ident!("pinned{at}");
            quote!(#name.clone())
        })
        .collect();
    let bindings = (0..last).map(|at| {
        let name = format_ident!("pinned{at}");
        let value = &pinned[at];
        quote!(let #name = #value;)
    });
    let moved: Vec<TokenStream> = (0..last)
        .map(|at| {
            let name = format_ident!("pinned{at}");
            quote!(#name)
        })
        .collect();

    // An end the caller left open falls back to the components pinned before the
    // one being ranged. Without it, `..= now` on `(status, due_at)` drops the
    // status from the low end and the scan walks into every status sorting
    // below it. With nothing pinned there is nothing to fall back to, and open
    // really is the end of the index.
    let pinned_width = last;
    let fallback = if last == 0 {
        quote!(None)
    } else {
        quote!(Some(::doclite::doclite_core::Key::from_columns([#(#moved,)*])))
    };

    quote! {
        #[doc = #doc]
        pub fn #method(
            self,
            #(#parameters,)*
            bounds: impl ::doclite::IntoBounds<#bound_type>,
        ) -> Self {
            #(#bindings)*
            let scan = match ::doclite::IntoBounds::into_bounds(bounds) {
                ::doclite::Bounds::Exact(last) => ::doclite::scan_prefix::<#arity, #width>(
                    &#record::#constant,
                    ::doclite::doclite_core::Key::from_columns([#(#cloned,)* last]),
                ),
                ::doclite::Bounds::Range { lo, hi } => ::doclite::scan_within::<
                    #arity,
                    #width,
                    #pinned_width,
                >(
                    &#record::#constant,
                    lo.map(|(last, bound)| (
                        ::doclite::doclite_core::Key::from_columns([#(#cloned,)* last]),
                        bound,
                    )),
                    hi.map(|(last, bound)| (
                        ::doclite::doclite_core::Key::from_columns([#(#cloned,)* last]),
                        bound,
                    )),
                    #fallback,
                ),
            };
            Self(self.0.on(scan))
        }
    }
}
