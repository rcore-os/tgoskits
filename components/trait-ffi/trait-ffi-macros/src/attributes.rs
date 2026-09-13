//! Derive item availability without evaluating the implementing crate's cfg.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, Meta, Result, Token, punctuated::Punctuated};

pub fn availability(attributes: &[Attribute]) -> Result<Vec<TokenStream>> {
    let mut predicates = Vec::new();
    for attribute in attributes {
        predicates.extend(meta_availability(&attribute.meta)?);
    }
    Ok(predicates)
}

fn meta_availability(meta: &Meta) -> Result<Vec<TokenStream>> {
    if meta.path().is_ident("cfg") {
        let Meta::List(list) = meta else {
            return Err(syn::Error::new_spanned(meta, "expected cfg(predicate)"));
        };
        let predicate = &list.tokens;
        return Ok(vec![quote!(#predicate)]);
    }
    if meta.path().is_ident("cfg_attr") {
        let Meta::List(list) = meta else {
            return Err(syn::Error::new_spanned(
                meta,
                "expected cfg_attr(predicate, attributes)",
            ));
        };
        let mut parts = list
            .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?
            .into_iter();
        let predicate = parts
            .next()
            .ok_or_else(|| syn::Error::new_spanned(meta, "cfg_attr requires a predicate"))?;
        let mut predicates = Vec::new();
        for attribute in parts {
            for condition in meta_availability(&attribute)? {
                // The nested attribute is applied only if its predicate holds.
                // Keeping this expression on the definition's helper macro
                // also handles nested cfg_attr and absent provider features.
                predicates.push(quote!(any(not(#predicate), #condition)));
            }
        }
        return Ok(predicates);
    }
    Ok(Vec::new())
}
