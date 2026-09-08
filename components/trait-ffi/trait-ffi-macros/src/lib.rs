//! Compile-time code generation for statically linked trait interfaces.

use proc_macro::TokenStream;
use syn::parse_macro_input;

mod args;
mod definition;
mod signature;

/// Define an interface, a calling module, and its scoped `impl_trait!` macro.
///
/// Methods must be synchronous associated functions with concrete types. The
/// generated implementation macro exports all methods, including inherited
/// defaults. See `trait-ffi` for the linking and safety contract.
#[proc_macro_attribute]
pub fn def_extern_trait(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as args::Args);
    let input = parse_macro_input!(input as syn::ItemTrait);
    definition::expand(args, input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
