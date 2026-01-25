//! Error definitions for the crate interface.

use syn::{Error, Generics, Ident, TraitItemFn};

pub fn duplicate_arg_error(ident: &Ident) -> Error {
    Error::new_spanned(ident, format!("duplicate argument: {}", ident))
}

pub fn unknown_arg_error(ident: &Ident) -> Error {
    Error::new_spanned(ident, format!("unknown argument: {}", ident))
}

pub fn generic_not_allowed_error(generic: &Generics) -> Error {
    Error::new_spanned(
        generic,
        "generic parameters are not allowed in crate_interface",
    )
}

#[cfg_attr(feature = "weak_default", allow(dead_code))]
pub fn weak_default_required_error(method: &TraitItemFn) -> Error {
    let fn_name = &method.sig.ident;
    Error::new_spanned(
        method,
        format!(
            r#"default implementation of method `{}` will not work as expected and therefore is not allowed without the `weak_default` feature. To use it, you need to enable the `weak_default` feature and use the nightly Rust toolchain, with `#![feature(linkage)]` at the top of your crate root."#,
            fn_name
        ),
    )
}
