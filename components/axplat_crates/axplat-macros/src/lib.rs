#![doc = include_str!("../README.md")]

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{Error, FnArg, ItemFn, ItemTrait, ReturnType, TraitItem};

fn compiler_error(err: Error) -> TokenStream {
    err.to_compile_error().into()
}

fn common_main(item: TokenStream, arg_num: usize, export_name: &str, err_msg: &str) -> TokenStream {
    let main = syn::parse_macro_input!(item as ItemFn);
    let mut err = if let ReturnType::Type(_, ty) = &main.sig.output {
        quote! { #ty }.to_string() != "!"
    } else {
        true
    };

    let args = &main.sig.inputs;
    for arg in args.iter() {
        if let FnArg::Typed(pat) = arg {
            let ty = &pat.ty;
            if quote! { #ty }.to_string() != "usize" {
                err = true;
                break;
            }
        }
    }
    if args.len() != arg_num {
        err = true;
    }

    if err {
        compiler_error(Error::new(Span::call_site(), err_msg))
    } else {
        quote! {
            #[unsafe(export_name = #export_name)]
            #main
        }
        .into()
    }
}

/// Marks a function to be called on the primary core after the platform
/// initialization.
///
/// The function signature must be `fn(cpu_id: usize, arg: usize) -> !`, where
/// `cpu_id` is the logical CPU ID (0, 1, ..., N-1, N is the number of CPU
/// cores on the platform), and `arg` is passed from the bootloader (typically
/// the device tree blob address).
///
/// # Example
///
/// ```rust
/// # use axplat_macros as axplat;
/// #[axplat::main]
/// fn primary_main(cpu_id: usize, arg: usize) -> ! {
///     todo!() // Your code here
/// }
#[proc_macro_attribute]
pub fn main(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return compiler_error(Error::new(
            Span::call_site(),
            "expect an empty attribute or `#[axplat::main]`",
        ));
    };
    common_main(
        item,
        2,
        "__axplat_main",
        "expect a function with type `fn(cpu_id: usize, arg: usize) -> !`",
    )
}

/// Marks a function to be called on the secondary cores after the platform
/// initialization.
///
/// The function signature must be `fn(cpu_id: usize) -> !`, where `cpu_id` is
/// the logical CPU ID (0, 1, ..., N-1, N is the number of CPU cores on the
/// platform).
///
/// # Example
///
/// ```rust
/// # use axplat_macros as axplat;
/// #[axplat::secondary_main]
/// fn secondary_main(cpu_id: usize) -> ! {
///     todo!() // Your code here
/// }
#[proc_macro_attribute]
pub fn secondary_main(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return compiler_error(Error::new(
            Span::call_site(),
            "expect an empty attribute or `#[axplat::secondary_main]`",
        ));
    };
    common_main(
        item,
        1,
        "__axplat_secondary_main",
        "expect a function with type `fn(cpu_id: usize) -> !`",
    )
}

#[doc(hidden)]
#[proc_macro_attribute]
pub fn def_plat_interface(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return compiler_error(Error::new(
            Span::call_site(),
            "expect an empty attribute: `#[def_plat_interface]`",
        ));
    }

    let trait_ast = syn::parse_macro_input!(item as ItemTrait);
    let trait_name = &trait_ast.ident;

    let mut fn_list = vec![];
    for item in &trait_ast.items {
        if let TraitItem::Fn(method) = item {
            let attrs = &method.attrs;
            let sig = &method.sig;
            let fn_name = &sig.ident;

            let mut args = vec![];
            for arg in &sig.inputs {
                match arg {
                    FnArg::Receiver(_) => {
                        return compiler_error(Error::new_spanned(
                            arg,
                            "`self` is not allowed in the interface definition",
                        ));
                    }
                    FnArg::Typed(ty) => args.push(ty.pat.clone()),
                }
            }

            fn_list.push(quote! {
                #(#attrs)*
                #[inline]
                pub #sig {
                    crate::__priv::call_interface!(#trait_name::#fn_name, #(#args),* )
                }
            });
        }
    }

    quote! {
        #[crate::__priv::def_interface]
        #trait_ast

        #(#fn_list)*
    }
    .into()
}
