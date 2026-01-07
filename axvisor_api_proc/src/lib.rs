use proc_macro::TokenStream as TokenStream1;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned};
use syn::{Ident, spanned::Spanned};

/// Find the path to the `axvisor_api` crate.
fn axvisor_api_crate() -> TokenStream {
    match crate_name("axvisor_api") {
        Ok(FoundCrate::Itself) => quote! { crate },
        Ok(FoundCrate::Name(name)) => {
            let name = Ident::new(&name, Span::call_site());
            quote! { #name }
        }
        Err(_) => quote! { compile_error!("`axvisor_api` crate not found") },
    }
}

/// The namespace used for AxVisor APIs when calling `crate_interface` macros.
fn axvisor_api_namespace() -> Ident {
    const AXVISOR_API_NS: &str = "AxVisorApi";
    Ident::new(AXVISOR_API_NS, Span::call_site())
}

macro_rules! assert_empty_attr {
    ($attr:expr) => {
        if !$attr.is_empty() {
            return (quote_spanned! {
                TokenStream::from($attr).span() => compile_error!("This attribute does not accept any arguments")
            }).into();
        }
    };
}

/// Define an AxVisor API interface.
///
/// This macro is applied to a trait definition. It generates the necessary
/// boilerplate code to register the trait as an AxVisor API interface. No
/// arguments are accepted.
///
/// This macro uses `crate_interface::def_interface` internally.
#[proc_macro_attribute]
pub fn api_def(attr: TokenStream1, input: TokenStream1) -> TokenStream1 {
    assert_empty_attr!(attr);

    let axvisor_api_path = axvisor_api_crate();
    let ns = axvisor_api_namespace();
    let input: TokenStream = syn::parse_macro_input!(input as TokenStream);

    quote! {
        #[#axvisor_api_path::__priv::crate_interface::def_interface(gen_caller, namespace = #ns)]
        #input
    }
    .into()
}

/// Implement an AxVisor API interface.
///
/// This macro is applied to an `impl` block that implements a trait previously
/// defined with `api_def`. It generates the necessary boilerplate code to
/// register the implementation as an AxVisor API implementation. No arguments
/// are accepted.
///
/// This macro uses `crate_interface::impl_interface` internally.
#[proc_macro_attribute]
pub fn api_impl(attr: TokenStream1, input: TokenStream1) -> TokenStream1 {
    assert_empty_attr!(attr);

    let axvisor_api_path = axvisor_api_crate();
    let ns = axvisor_api_namespace();
    let input: TokenStream = syn::parse_macro_input!(input as TokenStream);

    quote! {
        #[#axvisor_api_path::__priv::crate_interface::impl_interface(namespace = #ns)]
        #input
    }
    .into()
}
