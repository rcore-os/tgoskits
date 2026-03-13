//！Macros for registering constructor functions for Rust under no_std, which is like __attribute__((constructor)) in C/C++.
//!
//! **DO NOT** use this crate directly. Use the [ctor_bare](https://docs.rs/ctor_bare) crate instead.
//!
//! After attching the `register_ctor` macro to the given function, a pointer pointing to it will be stored in the `.init_array` section.
//! When the program is loaded, this section will be linked into the binary. The `call_ctors` function in the `ctor_bare`
//! crate will call all the constructor functions in the `.init_array` section.
//!
//! See the documentation of the [ctor_bare](https://docs.rs/ctor_bare) crate for more details.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{Error, Item, parse_macro_input};

/// Register a constructor function to be called before `main`.
///
/// The function should have no input arguments and return nothing.
///
/// See the documentation of the [ctor_bare](https://docs.rs/ctor_bare) crate for more details.
#[proc_macro_attribute]
pub fn register_ctor(attr: TokenStream, function: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return Error::new(
            Span::call_site(),
            "expect an empty attribute: `#[register_ctor]`",
        )
        .to_compile_error()
        .into();
    }

    let item: Item = parse_macro_input!(function as Item);
    if let Item::Fn(func) = item {
        let name = &func.sig.ident;
        let name_str = name.to_string();
        let name_ident = format_ident!("_INIT_{}", name_str);
        let output = &func.sig.output;
        // Constructor functions should not have any return value.
        if let syn::ReturnType::Type(_, _) = output {
            return Error::new(
                Span::call_site(),
                "expect no return value for the constructor function",
            )
            .to_compile_error()
            .into();
        }
        let inputs = &func.sig.inputs;
        // Constructor functions should not have any input arguments.
        if !inputs.is_empty() {
            return Error::new(
                Span::call_site(),
                "expect no input arguments for the constructor function",
            )
            .to_compile_error()
            .into();
        }
        let block = &func.block;

        quote! {
            #[unsafe(link_section = ".init_array")]
            #[used]
            #[allow(non_upper_case_globals)]
            static #name_ident: extern "C" fn() = #name;

            #[unsafe(no_mangle)]
            #[allow(non_upper_case_globals)]
            pub extern "C" fn #name() {
                #block
            }
        }
        .into()
    } else {
        Error::new(Span::call_site(), "expect a function to be registered")
            .to_compile_error()
            .into()
    }
}
