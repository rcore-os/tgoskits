//! The implementation of the [`crate::def_interface`] attribute macro.

use proc_macro2::TokenStream;
#[cfg(feature = "weak_default")]
use quote::format_ident;
use quote::quote;
use syn::{parse_quote, Error, ItemTrait, TraitItem};
#[cfg(feature = "weak_default")]
use syn::{
    punctuated::Punctuated, visit_mut::VisitMut, Block, Expr, ExprPath, Ident, Path, PathSegment,
    Signature,
};

#[cfg(feature = "weak_default")]
use std::collections::HashMap;

use crate::args::DefInterfaceArgs;
use crate::errors::generic_not_allowed_error;
#[cfg(not(feature = "weak_default"))]
use crate::errors::weak_default_required_error;
use crate::naming::{
    alias_guard_name, extern_fn_mod_name, extern_fn_name, extract_caller_args, namespace_guard_name,
};
use crate::validator::validate_fn_signature;

/// Rewrite all references to `Self::some_method` in the default body.
///
/// Both direct calls (`Self::method(...)`) and indirect references (`Self::method` as value)
/// are handled uniformly by generating proxy functions. This simplifies the implementation
/// and ensures consistent behavior.
///
/// This is necessary because default implementations may reference other trait methods
/// using `Self::method_name` syntax. We need to rewrite these references to use the
/// extern function names so that they resolve to the correct (possibly overridden)
/// implementation at link time.
#[cfg(feature = "weak_default")]
fn rewrite_self_in_default_body(
    default_body: &Block,
    trait_name: &Ident,
    namespace: Option<&str>,
    method_signatures: &HashMap<String, Signature>,
) -> TokenStream {
    /// Visitor that rewrites `Self::method_name` references using proxy functions.
    struct SelfRefRewriter<'a> {
        trait_name: &'a Ident,
        namespace: Option<&'a str>,
        method_signatures: &'a HashMap<String, Signature>,
        /// Generated proxy functions (method_name -> proxy_fn_code)
        /// Each method only generates one proxy function
        generated_proxies: HashMap<String, TokenStream>,
    }

    impl SelfRefRewriter<'_> {
        /// Check if an expression is a `Self::method_name` path (two segments starting with "Self")
        fn is_self_method_path(expr: &Expr) -> Option<Ident> {
            if let Expr::Path(path_expr) = expr {
                let path = &path_expr.path;
                if path.segments.len() == 2 {
                    let first_seg = &path.segments[0];
                    let second_seg = &path.segments[1];
                    if first_seg.ident == "Self" {
                        return Some(second_seg.ident.clone());
                    }
                }
            }
            None
        }

        /// Get the proxy function name for a method
        fn proxy_name(method_name: &Ident) -> Ident {
            format_ident!("__self_proxy_{}", method_name)
        }

        /// Ensure a proxy function exists for the method, generating it if needed
        fn ensure_proxy_fn(&mut self, method_name: Ident) -> Option<Ident> {
            let method_key = method_name.to_string();

            // Return early if already generated
            if self.generated_proxies.contains_key(&method_key) {
                return Some(Self::proxy_name(&method_name));
            }

            // Generate new proxy function
            let sig = self.method_signatures.get(&method_key)?;
            let mod_name = extern_fn_mod_name(self.trait_name);
            let extern_fn = extern_fn_name(self.namespace, &self.trait_name, &method_name);
            let proxy_name = Self::proxy_name(&method_name);

            // Extract arguments for the call
            let caller_args = extract_caller_args(sig).ok()?;

            // Clone signature and rename
            let mut proxy_sig = sig.clone();
            proxy_sig.ident = proxy_name.clone();

            // Generate the proxy function
            let proxy_fn = quote! {
                #[allow(non_snake_case)]
                #proxy_sig {
                    unsafe { #mod_name :: #extern_fn ( #caller_args ) }
                }
            };

            self.generated_proxies.insert(method_key, proxy_fn);
            Some(proxy_name)
        }

        /// Replace a `Self::method` expression with a proxy function reference
        fn replace_with_proxy(&mut self, expr: &mut Expr, method_name: Ident) {
            if let Some(proxy_name) = self.ensure_proxy_fn(method_name) {
                *expr = Expr::Path(ExprPath {
                    attrs: vec![],
                    qself: None,
                    path: Path {
                        leading_colon: None,
                        segments: {
                            let mut segs = Punctuated::new();
                            segs.push(PathSegment::from(proxy_name));
                            segs
                        },
                    },
                });
            }
        }
    }

    impl VisitMut for SelfRefRewriter<'_> {
        fn visit_expr_mut(&mut self, expr: &mut Expr) {
            // First, recursively visit child expressions
            syn::visit_mut::visit_expr_mut(self, expr);

            // Handle any `Self::method` reference (both direct calls and value references)
            if let Some(method_name) = Self::is_self_method_path(expr) {
                self.replace_with_proxy(expr, method_name);
            }
        }
    }

    let mut body = default_body.clone();
    let mut rewriter = SelfRefRewriter {
        trait_name,
        namespace,
        method_signatures,
        generated_proxies: HashMap::new(),
    };
    rewriter.visit_block_mut(&mut body);

    // Insert proxy functions at the beginning of the block
    let proxy_fns: Vec<_> = rewriter.generated_proxies.into_values().collect();
    if proxy_fns.is_empty() {
        quote! { #body }
    } else {
        let stmts = &body.stmts;
        quote! {
            {
                #(#proxy_fns)*
                #(#stmts)*
            }
        }
    }
}

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

    // Collect all method signatures for use in rewriting Self::method references
    #[cfg(feature = "weak_default")]
    let mut method_signatures: HashMap<String, Signature> = HashMap::new();
    #[cfg(feature = "weak_default")]
    for item in &ast.items {
        if let TraitItem::Fn(method) = item {
            let sig = &method.sig;
            method_signatures.insert(sig.ident.to_string(), sig.clone());
        }
    }

    let mut extern_fn_list = vec![];
    let mut callers: Vec<TokenStream> = vec![];

    for item in &mut ast.items {
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

            // Reject default implementations when weak_default feature is not enabled
            #[cfg(not(feature = "weak_default"))]
            if method.default.is_some() {
                return Err(weak_default_required_error(method));
            }

            // Generate weak symbol function for methods with default implementations
            #[cfg(feature = "weak_default")]
            if let Some(default_body) = &mut method.default {
                let default_body_cleaned = rewrite_self_in_default_body(
                    default_body,
                    trait_name,
                    macro_arg.namespace.as_deref(),
                    &method_signatures,
                );
                let weak_default_impl = quote! {
                    #[allow(non_snake_case)]
                    #[linkage = "weak"]
                    #[no_mangle]
                    extern "Rust" #extern_fn_sig #default_body_cleaned
                };

                let caller_args = extract_caller_args(sig)?;
                *default_body = syn::parse2(quote! {{
                    #weak_default_impl

                    #extern_fn_name ( #caller_args )
                }})?;
            }

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
