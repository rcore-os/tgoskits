#![doc = include_str!("../README.md")]

use std::vec;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    parse::Error, parse_macro_input, parse_quote, punctuated::Punctuated, token::Comma, Expr,
    FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, ItemTrait, Pat, PathArguments, PathSegment,
    TraitItem, Type,
};

mod args;

use args::{CallInterface, DefInterfaceArgs, ImplInterfaceArgs};

fn compiler_error(err: Error) -> TokenStream {
    err.to_compile_error().into()
}

/// Generate a unique identifier to guard against aliasing of trait names.
fn alias_guard_name(trait_name: &Ident) -> Ident {
    format_ident!("__MustNotAnAlias__{}", trait_name)
}

/// Generate a unique identifier to enforce namespace matching between
/// `def_interface` and `impl_interface`.
fn namespace_guard_name(namespace: &str) -> Ident {
    format_ident!("__NamespaceGuard__{}", namespace)
}

/// Generate the extern function name (the symbol `def_interface` defines and
/// `impl_interface` implements), based on the optional namespace, trait name,
/// and function name.
fn extern_fn_name(namespace: Option<&str>, trait_name: &Ident, fn_name: &Ident) -> Ident {
    if let Some(ns) = namespace {
        format_ident!("__{}_{}_{}", ns, trait_name, fn_name)
    } else {
        format_ident!("__{}_{}", trait_name, fn_name)
    }
}

/// Generate the module name that contains the extern function declarations.
///
/// Namespaces are not included here because no two traits can have the same
/// name in the same module, so the generated module name will always be unique.
fn extern_fn_mod_name(trait_name: &Ident) -> Ident {
    format_ident!("__{}_mod", trait_name)
}

/// Define an crate interface.
///
/// This attribute should be added above the definition of a trait. All traits
/// that use the attribute cannot have the same name, unless they are assigned
/// different namespaces with `namespace = "..."` option.
///
/// It is not necessary to define it in the same crate as the implementation,
/// but it is required that these crates are linked together.
/// 
/// It is also possible to generate calling helper functions for each interface
/// function by enabling the `gen_caller` option.
///
/// See the [crate-level documentation](crate) for more details.
#[proc_macro_attribute]
pub fn def_interface(attr: TokenStream, item: TokenStream) -> TokenStream {
    let macro_arg = syn::parse_macro_input!(attr as DefInterfaceArgs);

    let mut ast = syn::parse_macro_input!(item as ItemTrait);
    let trait_name = &ast.ident;
    let vis = &ast.vis;

    let mod_name = extern_fn_mod_name(trait_name);

    let mut extern_fn_list = vec![];
    let mut callers: Vec<proc_macro2::TokenStream> = vec![];
    for item in &ast.items {
        if let TraitItem::Fn(method) = item {
            let sig = &method.sig;
            let fn_name = &sig.ident;

            let extern_fn_name =
                extern_fn_name(macro_arg.namespace.as_deref(), trait_name, fn_name);

            let mut extern_fn_sig = sig.clone();
            extern_fn_sig.ident = extern_fn_name.clone();
            extern_fn_sig.inputs = Punctuated::new();

            for arg in &method.sig.inputs {
                if let FnArg::Typed(_) = arg {
                    extern_fn_sig.inputs.push(arg.clone());
                }
            }

            extern_fn_list.push(quote! {
                pub #extern_fn_sig;
            });

            if macro_arg.gen_caller {
                let attrs = &method.attrs;
                let mut caller_fn_sig = sig.clone();
                caller_fn_sig.inputs = Punctuated::new();
                let mut caller_args: Punctuated<Expr, Comma> = Punctuated::new();

                for arg in &method.sig.inputs {
                    if let FnArg::Typed(t) = arg {
                        if let Pat::Ident(arg_ident) = &*t.pat {
                            caller_fn_sig.inputs.push(arg.clone());
                            caller_args.push(parse_quote! { #arg_ident });
                        } else {
                            return compiler_error(Error::new_spanned(
                                &t.pat,
                                "unexpected pattern in function argument",
                            ));
                        }
                    }
                }
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
    // should be provided to ensure that `impl_interface` have a namespace
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

    quote! {
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
    }
    .into()
}

/// Implement the interface for a struct.
///
/// This attribute should be added above the implementation of a trait for a
/// struct, and the trait must be defined with
/// [`#[def_interface]`](macro@crate::def_interface).
///
/// It is not necessary to implement it in the same crate as the definition, but
/// it is required that these crates are linked together.
/// 
/// The specified trait name must not be an alias to the originally defined
/// name; otherwise, it will result in a compile error.
///
/// ```rust,compile_fail
/// # use crate_interface::*;
/// #[def_interface]
/// trait MyIf {
///     fn foo();
/// }
///
/// use MyIf as MyIf2;
/// struct MyImpl;
/// #[impl_interface]
/// impl MyIf2 for MyImpl {
///     fn foo() {}
/// }
/// ```
/// 
/// It's also mandatory to match the namespace if one is specified when defining
/// the interface. For example, the following will result in a compile error:
/// 
/// ```rust,compile_fail
/// # use crate_interface::*;
/// #[def_interface(namespace = MyNs)]
/// trait MyIf {
///     fn foo();
/// }
/// 
/// struct MyImpl;
/// 
/// #[impl_interface(namespace = OtherNs)] // error: namespace does not match
/// impl MyIf for MyImpl {
///     fn foo() {}
/// }
/// ```
///
/// See the [crate-level documentation](crate) for more details.
#[proc_macro_attribute]
pub fn impl_interface(attr: TokenStream, item: TokenStream) -> TokenStream {
    let arg = syn::parse_macro_input!(attr as ImplInterfaceArgs);

    let mut ast = syn::parse_macro_input!(item as ItemImpl);
    let trait_name = if let Some((_, path, _)) = &ast.trait_ {
        &path.segments.last().unwrap().ident
    } else {
        return compiler_error(Error::new_spanned(ast, "expect a trait implementation"));
    };
    let impl_name = if let Type::Path(path) = &ast.self_ty.as_ref() {
        path.path.get_ident().unwrap()
    } else {
        return compiler_error(Error::new_spanned(ast, "expect a trait implementation"));
    };

    for item in &mut ast.items {
        if let ImplItem::Fn(method) = item {
            let (attrs, vis, sig, stmts) =
                (&method.attrs, &method.vis, &method.sig, &method.block.stmts);
            let fn_name = &sig.ident;
            let extern_fn_name =
                extern_fn_name(arg.namespace.as_deref(), trait_name, fn_name).to_string();

            let mut new_sig = sig.clone();
            new_sig.ident = format_ident!("{}", extern_fn_name);
            new_sig.inputs = Punctuated::new();

            let mut args = vec![];
            let mut has_self = false;
            for arg in &sig.inputs {
                match arg {
                    FnArg::Receiver(_) => has_self = true,
                    FnArg::Typed(ty) => {
                        args.push(ty.pat.clone());
                        new_sig.inputs.push(arg.clone());
                    }
                }
            }

            let call_impl = if has_self {
                quote! {
                    let _impl: #impl_name = #impl_name;
                    _impl.#fn_name( #(#args),* )
                }
            } else {
                quote! { #impl_name::#fn_name( #(#args),* ) }
            };

            let item = quote! {
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
            }
            .into();
            *method = syn::parse_macro_input!(item as ImplItemFn);
        }
    }

    // generate alias guard to prevent aliasing of trait names
    let alias_guard_name = alias_guard_name(trait_name);
    let alias_guard = parse_quote!(const #alias_guard_name: () = (););
    ast.items.push(alias_guard);

    // generate namespace guard to enforce namespace matching
    if let Some(ns) = arg.namespace {
        let ns_guard_name = namespace_guard_name(&ns);
        let ns_guard = parse_quote!(const #ns_guard_name: () = (););
        ast.items.push(ns_guard);
    }

    quote! { #ast }.into()
}

/// Call a function in the interface.
///
/// It is not necessary to call it in the same crate as the implementation, but
/// it is required that these crates are linked together.
///
/// See the [crate-level documentation](crate) for more details.
#[proc_macro]
pub fn call_interface(item: TokenStream) -> TokenStream {
    let call = parse_macro_input!(item as CallInterface);
    let args = call.args;
    let mut path = call.path.segments;

    if path.len() < 2 {
        compiler_error(Error::new(Span::call_site(), "expect `Trait::func`"));
    }
    let fn_name = path.pop().unwrap();
    let trait_name = path.pop().unwrap();
    let extern_fn_name = extern_fn_name(
        call.namespace.as_deref(),
        &trait_name.value().ident,
        &fn_name.value().ident,
    );

    path.push_value(PathSegment {
        ident: extern_fn_mod_name(&trait_name.value().ident),
        arguments: PathArguments::None,
    });
    quote! { unsafe { #path :: #extern_fn_name( #args ) } }.into()
}
