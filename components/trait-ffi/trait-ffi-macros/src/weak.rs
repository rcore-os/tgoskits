//! Optional link-time defaults without requiring an explicit provider.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemTrait, Result, TraitItem, parse_quote};

use crate::{args::Args, signature};

pub fn expand(
    args: &Args,
    input: &ItemTrait,
    module: &syn::Ident,
    prefix: &str,
) -> Result<TokenStream> {
    if !args.weak_default {
        return Ok(TokenStream::new());
    }
    if input.unsafety.is_some() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "weak defaults require a safe trait; unsafe traits need an explicit provider",
        ));
    }
    let cfg = crate::attributes::availability(&input.attrs)?;
    let name = &input.ident;
    let abi = &args.abi;
    let mut forwarding = Vec::new();
    let mut defaults = Vec::new();
    let mut exports = Vec::new();
    for (index, item) in input.items.iter().enumerate() {
        let TraitItem::Fn(method) = item else {
            unreachable!("validated interface")
        };
        let attrs = &method.attrs;
        let mut sig = signature::named_signature(&method.sig, 0);
        let method_name = &sig.ident;
        let arguments: Vec<_> = sig
            .inputs
            .iter()
            .enumerate()
            .map(|(index, _)| format_ident!("__arg{index}"))
            .collect();
        let call = quote!(#module::#method_name(#(#arguments),*));
        let call = preserve_unsafety(&sig.safety, call);
        forwarding.push(quote!(#(#attrs)* #sig { #call }));
        if let Some(body) = &method.default {
            let mut fallback_name = format!("__trait_ffi_default_{index}");
            while input.items.iter().any(
                |item| matches!(item, TraitItem::Fn(method) if method.sig.ident == fallback_name),
            ) {
                fallback_name.push('_');
            }
            let fallback_name = format_ident!("{fallback_name}");
            let mut fallback_sig = method.sig.clone();
            fallback_sig.ident = fallback_name.clone();
            // The body stays in the original module and in an inherent impl.
            // `Self` therefore works even inside macros and nested items, while
            // Self::method resolves to the forwarding trait impl below.
            defaults.push(quote!(#(#attrs)* #fallback_sig #body));
            let symbol = format!("{prefix}_{}", method.sig.ident);
            sig.ident = format_ident!("__weak_{index}");
            sig.abi = Some(parse_quote!(extern #abi));
            let call = preserve_unsafety(
                &sig.safety,
                quote!(__DefaultDispatch::#fallback_name(#(#arguments),*)),
            );
            let ffi = (abi.value() == "C").then(|| quote!(#[deny(improper_ctypes_definitions)]));
            exports.push(quote! {
                #(#attrs)*
                #ffi
                #[linkage = "weak"]
                #[unsafe(export_name = #symbol)]
                #sig { #call }
            });
        }
    }
    Ok(quote! {
        #[cfg(all(#(#cfg),*))]
        const _: () = {
            struct __DefaultDispatch;
            impl #name for __DefaultDispatch { #(#forwarding)* }
            impl __DefaultDispatch { #(#defaults)* }
            #(#exports)*
        };
    })
}

fn preserve_unsafety(safety: &syn::Safety, call: TokenStream) -> TokenStream {
    if matches!(safety, syn::Safety::Unsafe(_)) {
        quote! {
            // SAFETY: the forwarding signature remains unsafe and has exactly
            // the original method's arguments and caller preconditions.
            unsafe { #call }
        }
    } else {
        call
    }
}
