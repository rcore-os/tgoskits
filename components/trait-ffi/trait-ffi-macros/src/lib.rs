//! Compile-time code generation for statically linked trait interfaces.

use proc_macro::TokenStream;
use syn::parse_macro_input;

mod args;
mod attributes;
mod call;
mod definition;
mod implementation;
mod signature;
mod weak;

/// Define an interface, a calling module, and its scoped `impl_trait!` macro.
///
/// Methods must be synchronous associated functions with concrete types. The
/// generated implementation macro exports all methods, including inherited
/// defaults. See `trait-ffi` for the linking and safety contract.
///
/// # Options
///
/// - `abi = "Rust"` (default) or `abi = "C"` selects the shared ABI.
/// - `mod_path = "platform"` locates a nested definition from its crate root.
/// - `module = "clock_api"` overrides the generated calling module name.
/// - `namespace = Board` adds an interface-owned link namespace.
/// - `gen_caller` re-exports calling functions beside the trait.
/// - `impl_macro = "bind_clock"` exposes an additional named binding macro.
/// - `weak_default` emits defaults without a provider, for safe traits only;
///   the defining crate must opt into nightly `#![feature(linkage)]`.
///
/// `cfg` and nested `cfg_attr` availability is selected by the defining crate.
/// Unsafe methods remain unsafe through every calling entry point.
#[proc_macro_attribute]
pub fn def_extern_trait(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as args::Args);
    let input = parse_macro_input!(input as syn::ItemTrait);
    definition::expand(args, input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Bind an ordinary concrete trait impl using the interface's own export macro.
///
/// Trait paths and import aliases are supported. The interface owns all link
/// metadata; this attribute accepts no independent ABI or namespace options.
#[proc_macro_attribute]
pub fn impl_extern_trait(args: TokenStream, input: TokenStream) -> TokenStream {
    if !args.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "impl_extern_trait accepts no options; link metadata belongs to the interface",
        )
        .into_compile_error()
        .into();
    }
    implementation::expand(parse_macro_input!(input as syn::ItemImpl))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Call `Interface::method(arguments)` or `Interface::method, arguments`.
///
/// Unsafe methods still require an unsafe block at the call site.
#[proc_macro]
pub fn call_interface(input: TokenStream) -> TokenStream {
    call::expand(parse_macro_input!(input as call::Call)).into()
}
