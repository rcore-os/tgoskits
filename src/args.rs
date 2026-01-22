//! Arguments definition and parsing for the `def_interface`, `impl_interface`
//! attributes and the `call_interface!` macro.

use syn::{
    parenthesized,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Expr, Ident, Path, Result, Token,
};

use crate::errors::{duplicate_arg_error, unknown_arg_error};

const KEY_GEN_CALLER: &str = "gen_caller";
const KEY_NAMESPACE: &str = "namespace";

/// Arguments for the `def_interface` attribute.
#[derive(Debug, Default)]
pub struct DefInterfaceArgs {
    /// Generate caller functions for members of the interface.
    pub gen_caller: bool,
    /// Namespace for the interface. Used to avoid name collisions and must
    /// match the one in `impl_interface`.
    pub namespace: Option<String>,
}

impl Parse for DefInterfaceArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut arg = DefInterfaceArgs::default();

        while !input.is_empty() {
            let ident: Ident = input.parse()?;

            match ident.to_string().as_str() {
                KEY_GEN_CALLER => {
                    if arg.gen_caller {
                        return Err(duplicate_arg_error(&ident));
                    }

                    arg.gen_caller = true;
                }
                KEY_NAMESPACE => {
                    if arg.namespace.is_some() {
                        return Err(duplicate_arg_error(&ident));
                    }

                    input.parse::<Token![=]>()?;
                    let ns_ident: Ident = input.parse()?;
                    arg.namespace = Some(ns_ident.to_string());
                }
                _ => {
                    return Err(unknown_arg_error(&ident));
                }
            }

            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(arg)
    }
}

/// Arguments for the `impl_interface` attribute.
#[derive(Debug, Default)]
pub struct ImplInterfaceArgs {
    /// Namespace for the interface. Used to avoid name collisions and must
    /// match the one in `def_interface`.
    pub namespace: Option<String>,
}

impl Parse for ImplInterfaceArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut arg = ImplInterfaceArgs::default();

        while !input.is_empty() {
            let ident: Ident = input.parse()?;

            match ident.to_string().as_str() {
                KEY_NAMESPACE => {
                    if arg.namespace.is_some() {
                        return Err(duplicate_arg_error(&ident));
                    }

                    input.parse::<Token![=]>()?;
                    let ns_ident: Ident = input.parse()?;
                    arg.namespace = Some(ns_ident.to_string());
                }
                _ => {
                    return Err(unknown_arg_error(&ident));
                }
            }

            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(arg)
    }
}

/// Arguments for the `call_interface!` macro.
pub struct CallInterface {
    /// Optional namespace for the interface.
    pub namespace: Option<String>,
    /// Path to the interface method to call.
    pub path: Path,
    /// Arguments to pass to the interface method.
    pub args: Punctuated<Expr, Token![,]>,
}

impl Parse for CallInterface {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut namespace = None;
        let content;

        let mut path: Path = input.parse()?;
        // try to parse namespace if any, we just assume that no programmer with
        // basic sanity would name a trait "namespace", and, anyway, a valid
        // path here requires at least 2 segments (Trait::func).
        if let Some(ident) = path.get_ident() {
            if ident == KEY_NAMESPACE {
                input.parse::<Token![=]>()?;
                let ns_ident: Ident = input.parse()?;
                namespace = Some(ns_ident.to_string());

                input.parse::<Token![,]>()?;
                path = input.parse()?;
            }
        }

        let args = if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            input.parse_terminated(Expr::parse, Token![,])?
        } else if !input.is_empty() {
            parenthesized!(content in input);
            content.parse_terminated(Expr::parse, Token![,])?
        } else {
            Punctuated::new()
        };
        Ok(CallInterface {
            namespace,
            path,
            args,
        })
    }
}
