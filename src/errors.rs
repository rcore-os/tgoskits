//! Error definitions for the crate interface.

use syn::{Error, Generics, Ident};

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
