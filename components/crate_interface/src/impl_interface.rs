//! The implementation of the [`crate::impl_interface`] attribute macro.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_quote, Error, ImplItem, ItemImpl, Type};

use crate::args::ImplInterfaceArgs;
use crate::naming::{alias_guard_name, extern_fn_name, extract_caller_args, namespace_guard_name};
use crate::validator::validate_fn_signature;

/// The implementation of the [`crate::impl_interface`] attribute macro.
pub fn impl_interface(
    mut ast: ItemImpl,
    macro_arg: ImplInterfaceArgs,
) -> Result<TokenStream, Error> {
    let trait_name = if let Some((_, path, _)) = &ast.trait_ {
        &path.segments.last().unwrap().ident
    } else {
        return Err(Error::new_spanned(ast, "expect a trait implementation"));
    };
    let impl_name = if let Type::Path(path) = &ast.self_ty.as_ref() {
        path.path.get_ident().unwrap()
    } else {
        return Err(Error::new_spanned(ast, "expect a trait implementation"));
    };

    for item in &mut ast.items {
        if let ImplItem::Fn(method) = item {
            let (attrs, vis, sig, stmts) =
                (&method.attrs, &method.vis, &method.sig, &method.block.stmts);
            let fn_name = &sig.ident;
            let extern_fn_name =
                extern_fn_name(macro_arg.namespace.as_deref(), trait_name, fn_name).to_string();

            // Validate signature: reject generic parameters and receivers
            validate_fn_signature(sig)?;

            let mut new_sig = sig.clone();
            new_sig.ident = format_ident!("{}", extern_fn_name);

            let args = extract_caller_args(sig)?;

            let call_impl = quote! { #impl_name::#fn_name( #args ) };

            let item: TokenStream = quote! {
                #[inline]
                #(#attrs)*
                #vis
                #sig
                {
                    {
                        #[inline]
                        #[export_name = #extern_fn_name]
                        extern "Rust" #new_sig {
                            #call_impl
                        }
                    }
                    #(#stmts)*
                }
            };
            *method = syn::parse2(item)?;
        }
    }

    // generate alias guard to prevent aliasing of trait names
    let alias_guard_name = alias_guard_name(trait_name);
    let alias_guard = parse_quote!(const #alias_guard_name: () = (););
    ast.items.push(alias_guard);

    // generate namespace guard to enforce namespace matching
    if let Some(ns) = macro_arg.namespace {
        let ns_guard_name = namespace_guard_name(&ns);
        let ns_guard = parse_quote!(const #ns_guard_name: () = (););
        ast.items.push(ns_guard);
    }

    Ok(quote! { #ast })
}
