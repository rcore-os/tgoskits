//! Bind a normal Rust impl through its interface's definition-owned exports.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ItemImpl, Result};

pub fn expand(input: ItemImpl) -> Result<TokenStream> {
    input.modifiers.require_empty()?;
    if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "trait-ffi bindings require a concrete provider, without impl generics or where \
             clauses",
        ));
    }
    let Some((interface, _)) = &input.trait_ else {
        return Err(syn::Error::new_spanned(
            &input.self_ty,
            "impl_extern_trait requires a trait implementation",
        ));
    };
    if interface
        .segments
        .iter()
        .any(|part| !part.arguments.is_empty())
    {
        return Err(syn::Error::new_spanned(
            interface,
            "trait-ffi interfaces cannot have generic arguments",
        ));
    }
    let provider = &input.self_ty;
    // The trait and its macro travel together through use/re-export aliases.
    // Namespace, ABI and signatures are owned by that macro, not this parser.
    let cfg = crate::attributes::availability(&input.attrs)?;
    Ok(quote! {
        #input
        #[cfg(all(#(#cfg),*))]
        #interface!(@bind #provider);
    })
}
