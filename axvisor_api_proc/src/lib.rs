// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Procedural macros for the `axvisor_api` crate.
//!
//! This crate provides the procedural macros used to define and implement
//! AxVisor API interfaces. These macros are built on top of the
//! `crate_interface` crate and provide a convenient way to create
//! link-time-resolved API interfaces.
//!
//! # Macros
//!
//! - [`api_def`] - Define an API interface trait.
//! - [`api_impl`] - Implement an API interface.
//!
//! # Usage
//!
//! This crate is re-exported by `axvisor_api` and should not be used directly.
//! Instead, use the macros through `axvisor_api`:
//!
//! ```rust,ignore
//! use axvisor_api::{api_def, api_impl};
//!
//! #[api_def]
//! pub trait MyApiIf {
//!     fn my_function() -> u32;
//! }
//!
//! struct MyApiImpl;
//!
//! #[api_impl]
//! impl MyApiIf for MyApiImpl {
//!     fn my_function() -> u32 {
//!         42
//!     }
//! }
//! ```
//!
//! # How It Works
//!
//! The macros use `crate_interface` under the hood, which leverages Rust's
//! link-time symbol resolution to connect API definitions with their
//! implementations. This allows for a cleaner API without explicit generic
//! parameters.

use proc_macro::TokenStream as TokenStream1;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned};
use syn::{Ident, spanned::Spanned};

/// Find the path to the `axvisor_api` crate.
///
/// This function determines the correct path to use when referring to
/// `axvisor_api` from within the generated code, handling both the case
/// where we're inside the `axvisor_api` crate itself and when we're in
/// an external crate.
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

/// Get the namespace identifier used for AxVisor APIs.
///
/// All AxVisor APIs share a common namespace to avoid conflicts with other
/// uses of `crate_interface`.
fn axvisor_api_namespace() -> Ident {
    const AXVISOR_API_NS: &str = "AxVisorApi";
    Ident::new(AXVISOR_API_NS, Span::call_site())
}

/// Macro to assert that an attribute has no arguments.
macro_rules! assert_empty_attr {
    ($attr:expr) => {
        if !$attr.is_empty() {
            return (quote_spanned! {
                TokenStream::from($attr).span() => compile_error!("This attribute does not accept any arguments")
            })
            .into();
        }
    };
}

/// Define an AxVisor API interface.
///
/// This attribute macro is applied to a trait definition to register it as
/// an AxVisor API interface. It generates caller functions for each method
/// in the trait, allowing the API to be called as regular functions.
///
/// # Usage
///
/// ```rust,ignore
/// use axvisor_api::api_def;
///
/// #[api_def]
/// pub trait MyApiIf {
///     /// Get a value.
///     fn get_value() -> u32;
///
///     /// Set a value.
///     fn set_value(value: u32);
/// }
///
/// // After the macro expansion, you can call:
/// // my_module::get_value()
/// // my_module::set_value(42)
/// ```
///
/// # Generated Code
///
/// The macro generates:
/// 1. The original trait definition with `crate_interface::def_interface`
///    attribute.
/// 2. Free-standing caller functions for each trait method at the same
///    module level.
///
/// # Attributes
///
/// This macro does not accept any arguments.
///
/// # Implementation
///
/// This macro uses `crate_interface::def_interface` internally with the
/// `gen_caller` option to generate the caller functions.
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
/// This attribute macro is applied to an `impl` block that implements a
/// trait previously defined with [`api_def`]. It registers the implementation
/// so that calls to the API functions are resolved to this implementation
/// at link time.
///
/// # Usage
///
/// ```rust,ignore
/// use axvisor_api::{api_def, api_impl};
///
/// #[api_def]
/// pub trait MyApiIf {
///     fn get_value() -> u32;
/// }
///
/// struct MyApiImpl;
///
/// #[api_impl]
/// impl MyApiIf for MyApiImpl {
///     fn get_value() -> u32 {
///         42
///     }
/// }
/// ```
///
/// # Requirements
///
/// - The implemented trait must have been defined with [`api_def`].
/// - The implementing type should be an empty struct (marker type).
/// - Only one implementation per API trait is allowed in the final binary.
///
/// # Attributes
///
/// This macro does not accept any arguments.
///
/// # Implementation
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
