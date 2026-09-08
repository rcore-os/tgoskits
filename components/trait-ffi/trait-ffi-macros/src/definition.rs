//! Generate declarations and definition-owned export macros from one contract.

use convert_case::{Case, Casing};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Attribute, ItemTrait, Meta, Result, TraitItem};

use crate::{
    args::{Args, link_prefix},
    signature,
};

pub fn expand(args: Args, input: ItemTrait) -> Result<TokenStream> {
    if !input.generics.params.is_empty()
        || input.generics.where_clause.is_some()
        || !input.supertraits.is_empty()
    {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "trait-ffi interfaces cannot have generics, where clauses or supertraits",
        ));
    }
    input.modifiers.require_empty()?;
    let name = &input.ident;
    let vis = &input.vis;
    let unsafety = &input.unsafety;
    let module = format_ident!(
        "{}",
        name.to_string()
            .trim_start_matches("r#")
            .to_case(Case::Snake)
    );
    let prefix = link_prefix(&args, name)?;
    let impl_macro = format_ident!("{prefix}_impl");
    let public_alias = args
        .impl_macro
        .as_ref()
        .map(|alias| quote!(pub use #impl_macro as #alias;));
    let owner = args
        .mod_path
        .as_ref()
        .map_or(quote!($crate), |path| quote!($crate::#path));
    let abi = &args.abi;
    let ffi_declarations = (abi.value() == "C").then(|| quote!(#[deny(improper_ctypes)]));
    let ffi_definitions =
        (abi.value() == "C").then(|| quote!(#[deny(improper_ctypes_definitions)]));
    let mut helpers = Vec::new();
    let mut methods = Vec::new();
    let mut exports = Vec::new();
    let mut helper_aliases = Vec::new();
    let trait_cfg = cfg_predicates(&input.attrs)?;

    for (index, item) in input.items.iter().enumerate() {
        let TraitItem::Fn(method) = item else {
            return Err(syn::Error::new_spanned(
                item,
                "trait-ffi interfaces only support associated functions",
            ));
        };
        method.modifiers.require_empty()?;
        signature::validate(&method.sig)?;
        let mut predicates = trait_cfg.clone();
        predicates.extend(cfg_predicates(&method.attrs)?);
        let enabled = quote!(all(#(#predicates),*));
        let method_name = &method.sig.ident;
        let symbol = format!("{prefix}_{method_name}");
        let helper = format_ident!("{prefix}_export_{index}");
        let types = format_ident!("__method_{index}");
        let path = quote!(#owner::#module::#types);
        let signature::ExportSignature {
            aliases,
            inputs,
            output,
            lifetimes,
            arguments,
        } = signature::export_signature(&method.sig, &path)?;
        let generics = if lifetimes.is_empty() {
            quote!()
        } else {
            quote!(<#(#lifetimes),*>)
        };
        let safety = &method.sig.safety;
        let call = quote!(<$provider as #owner::#name>::#method_name(#(#arguments),*));
        let call = if matches!(safety, syn::Safety::Unsafe(_)) {
            quote!({
                // SAFETY: this shim preserves the interface's unsafe signature;
                // the caller must satisfy the same method preconditions.
                unsafe { #call }
            })
        } else {
            call
        };
        helpers.push(quote! {
            #[doc(hidden)]
            #[cfg(#enabled)]
            #[macro_export]
            macro_rules! #helper {
                ($provider:ty) => {
                    const _: () = {
                        #ffi_definitions
                        #[unsafe(export_name = #symbol)]
                        #safety extern #abi fn __export #generics (#(#inputs),*) #output {
                            #call
                        }
                    };
                };
            }
            #[doc(hidden)]
            #[cfg(not(#enabled))]
            #[macro_export]
            macro_rules! #helper { ($provider:ty) => {}; }
        });
        let helper_alias = format_ident!("__export_{index}");
        helper_aliases.push(quote!(#[doc(hidden)] pub use #helper as #helper_alias;));
        exports.push(quote!(#owner::#module::#helper_alias!($provider);));
        let attrs = &method.attrs;
        let sig = signature::caller_signature(&method.sig);
        let mut foreign_sig = sig.clone();
        foreign_sig.ident = format_ident!("__invoke");
        // Foreign declarations are unsafe to call even for safe interface
        // methods. The wrapper discharges only the generated-link obligation.
        foreign_sig.safety = syn::Safety::Default;
        methods.push(quote! {
            #[doc(hidden)]
            #[cfg(#enabled)]
            pub mod #types {
                use super::super::*;
                #aliases
            }
            #(#attrs)*
            #[inline]
            pub #sig {
                #ffi_declarations
                unsafe extern #abi {
                    #[link_name = #symbol]
                    #foreign_sig;
                }
                // SAFETY: the definition-owned implementation macro exports
                // this exact signature and ABI for the same interface identity.
                // Unsafe method preconditions remain on the public wrapper.
                unsafe { __invoke(#(#arguments),*) }
            }
        });
    }

    Ok(quote! {
        #input
        #(#helpers)*
        #public_alias
        #[doc(hidden)]
        #[cfg(all(#(#trait_cfg),*))]
        #[macro_export]
        macro_rules! #impl_macro {
            (#unsafety impl #name for $provider:ty { $($body:tt)* }) => {
                #unsafety impl #owner::#name for $provider { $($body)* }
                #(#exports)*
            };
        }
        #[cfg(all(#(#trait_cfg),*))]
        #vis mod #module {
            use super::*;
            mod __macros {
                /// Implement and export this interface exactly once per final link.
                pub use #impl_macro as impl_trait;
                #(#helper_aliases)*
            }
            pub use self::__macros::*;
            #(#methods)*
        }
    })
}

fn cfg_predicates(attrs: &[Attribute]) -> Result<Vec<TokenStream>> {
    attrs
        .iter()
        .filter_map(|attr| {
            if attr.path().is_ident("cfg_attr") {
                return Some(Err(syn::Error::new_spanned(
                    attr,
                    "trait-ffi requires direct cfg attributes; cfg_attr is not supported",
                )));
            }
            if attr.path().is_ident("cfg") {
                return Some(match &attr.meta {
                    Meta::List(list) => Ok(list.tokens.clone()),
                    _ => Err(syn::Error::new_spanned(attr, "expected cfg(predicate)")),
                });
            }
            None
        })
        .collect()
}
