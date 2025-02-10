use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};

use syn::{parse_macro_input, spanned::Spanned, Ident, ItemStruct, Lifetime, LifetimeParam};

#[proc_macro_derive(ProviderSerde)]
pub fn provider_serde(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let item = parse_macro_input!(item as ItemStruct);

    imp(item).unwrap_or_else(|err| err).into()
}

fn imp(input: ItemStruct) -> Result<TokenStream, TokenStream> {
    let ident = &input.ident;
    let name = input.ident.to_string();
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    let mut de_generics = input.generics.clone();
    de_generics.params.insert(
        0,
        syn::GenericParam::Lifetime(LifetimeParam::new(Lifetime::new("'de", input.ident.span()))),
    );

    let mut repr_generics = input.generics.clone();
    repr_generics.params.insert(
        0,
        syn::GenericParam::Lifetime(LifetimeParam::new(Lifetime::new("'a", input.ident.span()))),
    );

    let visitor_name = Ident::new(&format!("__{}Visitor", ident), ident.span());
    let repr_name = Ident::new(&format!("__{}Repr", ident), ident.span());

    let mut repr_fields = Vec::new();
    let mut to_repr_assigners = Vec::new();
    let mut from_repr_assigners = Vec::new();
    for field in &input.fields {
        let field_ident = &field.ident;
        let field_ty = &field.ty;

        repr_fields.push(quote_spanned! { field.span() =>
            #field_ident: ::std::borrow::Cow<'a, #field_ty>
        });
        to_repr_assigners.push(quote_spanned! { field.span() =>
            #field_ident: ::std::borrow::Cow::Borrowed(&self.#field_ident)
        });
        from_repr_assigners.push(quote_spanned! { field.span() =>
            #field_ident: repr.#field_ident.into_owned()
        });
    }

    Ok(quote! {
        #[derive(Clone, Serialize, Deserialize)]
        struct #repr_name #repr_generics #where_clause {
            #( #repr_fields, )*
        }

        impl #impl_generics ::serde::Serialize for #ident #type_generics #where_clause {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: ::serde::Serializer,
            {
                use ::serde::ser::SerializeMap;

                let mut serializer = serializer.serialize_map(Some(1))?;
                serializer.serialize_key(crate::rule::PROVIDER_SENTINEL)?;
                let repr = #repr_name :: #type_generics {
                    #( #to_repr_assigners, )*
                };
                serializer.serialize_value(&crate::rule::ProviderRepr {
                    // FIXME: don't hard code
                    source_label: ::std::borrow::Cow::Borrowed(Label::new("@com.cealn.builtin//:exec.py").unwrap()),
                    qualname: ::std::borrow::Cow::Borrowed(#name),
                    data: ::std::borrow::Cow::Borrowed(&repr),
                })?;
                serializer.end()
            }
        }

        impl #de_generics ::serde::Deserialize<'de> for #ident #type_generics #where_clause {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                deserializer.deserialize_map(#visitor_name :: #type_generics { _phantom: ::std::marker::PhantomData })
            }
        }

        struct #visitor_name #type_generics #where_clause {
            _phantom: ::std::marker::PhantomData #type_generics
        }

        impl #de_generics ::serde::de::Visitor<'de> for #visitor_name #type_generics #where_clause {
            type Value = #ident #type_generics;

            fn expecting(&self, formatter: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
                formatter.write_str("typed provider")
            }

            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: ::serde::de::MapAccess<'de>,
            {
                if let Some((key, value)) = access.next_entry::<String, crate::rule::ProviderRepr<#repr_name :: #type_generics >>()? {
                    if key == crate::rule::PROVIDER_SENTINEL {
                        let repr = value.data.into_owned();
                        return Ok(#ident :: #type_generics {
                            #( #from_repr_assigners, )*
                        });
                    }
                }
                return Err(<M::Error as ::serde::de::Error>::custom("expected $cealn_provider tag"));
            }
        }
    })
}
