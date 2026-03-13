#![doc = include_str!("../README.md")]
#![allow(clippy::needless_doctest_main)]

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{parse::Error, parse_macro_input, ItemImpl, ItemTrait, PathArguments, PathSegment};

mod args;
mod def_interface;
mod errors;
mod impl_interface;
mod naming;
mod validator;

use args::{CallInterface, DefInterfaceArgs, ImplInterfaceArgs};
use naming::{extern_fn_mod_name, extern_fn_name};

fn compiler_error(err: Error) -> TokenStream {
    err.to_compile_error().into()
}

/// Define a crate interface.
///
/// This attribute should be added above the definition of a trait. All traits
/// that use the attribute cannot have the same name, unless they are assigned
/// different namespaces with `namespace = "..."` option.
///
/// It is not necessary to define it in the same crate as the implementation,
/// but it is required that these crates are linked together.
///
/// See the [crate-level documentation](crate) for more details.
///
/// ## Calling Helper Functions
///
/// It is also possible to generate calling helper functions for each interface
/// function by enabling the `gen_caller` option.
///
/// ## Restrictions
///
/// ### No Receivers
///
/// Methods with receivers (`self`, `&self`, `&mut self`) are not
/// allowed. Only associated functions (static methods) are supported:
///
/// ```rust,compile_fail
/// # use crate_interface::*;
/// #[def_interface]
/// trait MyIf {
///     fn foo(&self); // error: methods with receiver (self) are not allowed
/// }
/// ```
///
/// ### No Generic Parameters
///
/// Generic parameters are not supported. Interface functions cannot
/// have generic type parameters, lifetime parameters, or const generic
/// parameters:
///
/// ```rust,compile_fail
/// # use crate_interface::*;
/// #[def_interface]
/// trait MyIf {
///     fn foo<T>(x: T); // error: generic parameters are not allowed
/// }
/// ```
#[proc_macro_attribute]
pub fn def_interface(attr: TokenStream, item: TokenStream) -> TokenStream {
    let macro_arg = syn::parse_macro_input!(attr as DefInterfaceArgs);
    let ast = syn::parse_macro_input!(item as ItemTrait);

    def_interface::def_interface(ast, macro_arg)
        .map(Into::into)
        .unwrap_or_else(compiler_error)
}

/// Implement the crate interface for a struct.
///
/// This attribute should be added above the implementation of a trait for a
/// struct, and the trait must be defined with
/// [`#[def_interface]`](macro@crate::def_interface).
///
/// It is not necessary to implement it in the same crate as the definition, but
/// it is required that these crates are linked together.
///
/// See the [crate-level documentation](crate) for more details.
///
/// ## Restrictions
///
/// ### No Alias
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
/// ### No Namespace Mismatch
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
/// ### No Receivers
///
/// Methods with receivers (`self`, `&self`, `&mut self`) are not
/// allowed in the implementation either:
///
/// ```rust,compile_fail
/// # use crate_interface::*;
/// trait MyIf {
///     fn foo(&self);
/// }
///
/// struct MyImpl;
///
/// #[impl_interface]
/// impl MyIf for MyImpl {
///     fn foo(&self) {} // error: methods with receiver (self) are not allowed
/// }
/// ```
///
/// ### No Generic Parameters
///
/// Generic parameters are not supported in the implementation either:
///
/// ```rust,compile_fail
/// # use crate_interface::*;
/// trait MyIf {
///     fn foo<T>(x: T);
/// }
///
/// struct MyImpl;
///
/// #[impl_interface]
/// impl MyIf for MyImpl {
///     fn foo<T>(x: T) {} // error: generic parameters are not allowed
/// }
/// ```
#[proc_macro_attribute]
pub fn impl_interface(attr: TokenStream, item: TokenStream) -> TokenStream {
    let arg = syn::parse_macro_input!(attr as ImplInterfaceArgs);
    let ast = syn::parse_macro_input!(item as ItemImpl);

    impl_interface::impl_interface(ast, arg)
        .map(Into::into)
        .unwrap_or_else(compiler_error)
}

/// Call a function in a crate interface.
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
