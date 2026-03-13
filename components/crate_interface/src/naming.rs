//! Naming utilities for the crate interface.

use quote::format_ident;
use syn::{
    parse_quote, punctuated::Punctuated, token::Comma, Error, Expr, FnArg, Ident, Pat, Signature,
};

/// Extract the argument list from the function signature to be used by the caller.
///
/// Returns `Err(Error)` with a compile error if any argument is not an identifier.
///
/// Receivers are ignored because they are already rejected by `validate_fn_signature`.
///
/// Returns `Ok(Punctuated<Expr, Comma>)` with the argument list.
pub fn extract_caller_args(sig: &Signature) -> Result<Punctuated<Expr, Comma>, Error> {
    let mut args = Punctuated::new();
    for arg in &sig.inputs {
        if let FnArg::Typed(t) = arg {
            if let Pat::Ident(arg_ident) = &*t.pat {
                args.push(parse_quote! { #arg_ident });
            } else {
                return Err(Error::new_spanned(
                    &t.pat,
                    "unexpected pattern in function argument",
                ));
            }
        }
    }
    Ok(args)
}

/// Generate a unique identifier to guard against aliasing of trait names.
pub fn alias_guard_name(trait_name: &Ident) -> Ident {
    format_ident!("__MustNotAnAlias__{}", trait_name)
}

/// Generate a unique identifier to enforce namespace matching between
/// `def_interface` and `impl_interface`.
pub fn namespace_guard_name(namespace: &str) -> Ident {
    format_ident!("__NamespaceGuard__{}", namespace)
}

/// Generate the extern function name (the symbol `def_interface` defines and
/// `impl_interface` implements), based on the optional namespace, trait name,
/// and function name.
pub fn extern_fn_name(namespace: Option<&str>, trait_name: &Ident, fn_name: &Ident) -> Ident {
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
pub fn extern_fn_mod_name(trait_name: &Ident) -> Ident {
    format_ident!("__{}_mod", trait_name)
}
