//! The implementation of the [`crate::def_interface`] attribute macro.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{parse_quote, Error, ItemTrait, TraitItem};

use crate::args::DefInterfaceArgs;
use crate::errors::generic_not_allowed_error;
use crate::naming::{
    alias_guard_name, extern_fn_mod_name, extern_fn_name, extract_caller_args, namespace_guard_name,
};
use crate::validator::validate_fn_signature;

/// The implementation of the [`crate::def_interface`] attribute macro.
pub fn def_interface(
    mut ast: ItemTrait,
    macro_arg: DefInterfaceArgs,
) -> Result<TokenStream, Error> {
    let trait_name = &ast.ident;
    let vis = &ast.vis;

    if !ast.generics.params.is_empty() {
        return Err(generic_not_allowed_error(&ast.generics));
    }

    let mod_name = extern_fn_mod_name(trait_name);

    let mut extern_fn_list = vec![];
    let mut callers: Vec<proc_macro2::TokenStream> = vec![];
    for item in &ast.items {
        if let TraitItem::Fn(method) = item {
            let sig = &method.sig;
            let fn_name = &sig.ident;

            // Validate signature: reject generic parameters and receivers
            validate_fn_signature(sig)?;

            let extern_fn_name =
                extern_fn_name(macro_arg.namespace.as_deref(), trait_name, fn_name);

            let mut extern_fn_sig = sig.clone();
            extern_fn_sig.ident = extern_fn_name.clone();

            extern_fn_list.push(quote! {
                pub #extern_fn_sig;
            });

            if macro_arg.gen_caller {
                let attrs = &method.attrs;
                let caller_fn_sig = sig.clone();
                let caller_args = extract_caller_args(sig)?;
                callers.push(quote! {
                    #(#attrs)*
                    #[inline]
                    #vis #caller_fn_sig {
                        unsafe { #mod_name :: #extern_fn_name ( #caller_args ) }
                    }
                })
            }
        }
    }

    // Enforce no alias is used to implement an interface, as this makes it
    // possible to link the function called by `call_interface` to an
    // implementation with a different signature, which is extremely unsound.
    let alias_guard_name = alias_guard_name(trait_name);
    let alias_guard = parse_quote!(
        #[allow(non_upper_case_globals)]
        #[doc(hidden)]
        const #alias_guard_name: () = ();
    );
    ast.items.push(alias_guard);

    // Enforce namespace matching if a namespace is specified. No default value
    // should be provided to ensure that `impl_interface` has a namespace
    // specified when `def_interface` has one.
    if let Some(ns) = &macro_arg.namespace {
        let ns_guard_name = namespace_guard_name(ns);
        let ns_guard = parse_quote!(
            #[allow(non_upper_case_globals)]
            #[doc(hidden)]
            const #ns_guard_name: ();
        );
        ast.items.push(ns_guard);
    }

    Ok(quote! {
        #ast

        #[doc(hidden)]
        #[allow(non_snake_case)]
        #vis mod #mod_name {
            use super::*;
            extern "Rust" {
                #(#extern_fn_list)*
            }
        }

        #(#callers)*
    })
}
