//! Strict parsing and unambiguous link identities.

use proc_macro2::Span;
use syn::{
    Ident, LitStr, Path, Result, Token,
    parse::{Parse, ParseStream},
};

pub struct Args {
    pub abi: LitStr,
    pub mod_path: Option<Path>,
    pub namespace: Option<LitStr>,
    pub impl_macro: Option<Ident>,
    pub module: Option<Ident>,
    pub gen_caller: bool,
    pub weak_default: bool,
}

impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut args = Self {
            abi: LitStr::new("Rust", Span::call_site()),
            mod_path: None,
            namespace: None,
            impl_macro: None,
            module: None,
            gen_caller: false,
            weak_default: false,
        };
        let mut seen = std::collections::HashSet::new();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            if !seen.insert(key.to_string()) {
                return Err(syn::Error::new_spanned(key, "duplicate trait-ffi option"));
            }
            if key == "gen_caller" || key == "weak_default" {
                if key == "gen_caller" {
                    args.gen_caller = true;
                } else {
                    args.weak_default = true;
                }
                if !input.is_empty() {
                    input.parse::<Token![,]>()?;
                }
                continue;
            }
            input.parse::<Token![=]>()?;
            let value: LitStr = if key == "namespace" && !input.peek(LitStr) {
                let namespace: Ident = input.parse()?;
                LitStr::new(&namespace.to_string(), namespace.span())
            } else {
                input.parse()?
            };
            match key.to_string().as_str() {
                "abi" => {
                    let abi = match value.value().as_str() {
                        "rust" | "Rust" => "Rust",
                        "c" | "C" => "C",
                        _ => return Err(syn::Error::new_spanned(value, "ABI must be Rust or C")),
                    };
                    args.abi = LitStr::new(abi, value.span());
                }
                "mod_path" => {
                    let path: Path = value.parse()?;
                    if path.leading_colon.is_some()
                        || path.segments.iter().any(|segment| {
                            !segment.arguments.is_empty()
                                || matches!(
                                    segment.ident.to_string().as_str(),
                                    "crate" | "self" | "super"
                                )
                        })
                    {
                        return Err(syn::Error::new_spanned(
                            value,
                            "mod_path must be a path relative to the interface crate root",
                        ));
                    }
                    args.mod_path = Some(path);
                }
                "namespace" => {
                    let _: Ident = value.parse()?;
                    args.namespace = Some(value);
                }
                "impl_macro" => args.impl_macro = Some(value.parse()?),
                "module" => args.module = Some(value.parse()?),
                _ => {
                    return Err(syn::Error::new_spanned(
                        key,
                        "unknown trait-ffi option; expected abi, mod_path, namespace, module, \
                         impl_macro, gen_caller or weak_default",
                    ));
                }
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(args)
    }
}

pub fn link_prefix(args: &Args, name: &Ident) -> Result<String> {
    let package = std::env::var("CARGO_PKG_NAME")
        .map_err(|error| syn::Error::new(name.span(), format!("missing package name: {error}")))?;
    let version = std::env::var("CARGO_PKG_VERSION").map_err(|error| {
        syn::Error::new(name.span(), format!("missing package version: {error}"))
    })?;
    let version = semver::Version::parse(&version).map_err(|error| {
        syn::Error::new(name.span(), format!("invalid package version: {error}"))
    })?;
    let compatibility = if !version.pre.is_empty() {
        version.to_string()
    } else if version.major > 0 {
        version.major.to_string()
    } else if version.minor > 0 {
        format!("0_{}", version.minor)
    } else {
        format!("0_0_{}", version.patch)
    };
    // Hex encoding preserves package punctuation and module separators without
    // conflating identifiers such as `a_b::c` and `a::b_c`.
    let path = args
        .mod_path
        .as_ref()
        .map(|path| {
            path.segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::")
        })
        .unwrap_or_default();
    let identity = format!(
        "{package}/{compatibility}/{path}/{}/{name}",
        args.namespace
            .as_ref()
            .map(LitStr::value)
            .unwrap_or_default()
    );
    use std::fmt::Write;
    let mut prefix = String::from("__trait_ffi_v1_");
    for byte in identity.bytes() {
        write!(prefix, "{byte:02x}").expect("writing into a String cannot fail");
    }
    Ok(prefix)
}
