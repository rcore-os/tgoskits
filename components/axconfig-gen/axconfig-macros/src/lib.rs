#![cfg_attr(feature = "nightly", feature(proc_macro_expand))]
#![doc = include_str!("../README.md")]

use proc_macro::{LexError, TokenStream};
use quote::{quote, ToTokens};
use syn::parse::{Parse, ParseStream};
use syn::parse_macro_input;
use syn::{Error, Ident, LitStr, Result, Token};

use axconfig_gen::{Config, OutputFormat};

fn compiler_error<T: ToTokens>(tokens: T, msg: String) -> TokenStream {
    Error::new_spanned(tokens, msg).to_compile_error().into()
}

/// Parses TOML config content and expands it into Rust code.
///
/// # Example
///
/// See the [crate-level documentation][crate].
#[proc_macro]
pub fn parse_configs(config_toml: TokenStream) -> TokenStream {
    #[cfg(feature = "nightly")]
    let config_toml = match config_toml.expand_expr() {
        Ok(s) => s,
        Err(e) => {
            return Error::new(proc_macro2::Span::call_site(), e.to_string())
                .to_compile_error()
                .into()
        }
    };

    let config_toml = parse_macro_input!(config_toml as LitStr).value();
    let code = Config::from_toml(&config_toml).and_then(|cfg| cfg.dump(OutputFormat::Rust));
    match code {
        Ok(code) => code
            .parse()
            .unwrap_or_else(|e: LexError| compiler_error(config_toml, e.to_string())),
        Err(e) => compiler_error(config_toml, e.to_string()),
    }
}

/// Includes a TOML format config file and expands it into Rust code.
///
/// There a three ways to specify the path to the config file, either through the
/// path itself or through an environment variable.
///
/// ```rust,ignore
/// include_configs!("path/to/config.toml");
/// // or specify the config file path via an environment variable
/// include_configs!(path_env = "AX_CONFIG_PATH");
/// // or with a fallback path if the environment variable is not set
/// include_configs!(path_env = "AX_CONFIG_PATH", fallback = "path/to/defconfig.toml");
/// ```
///
/// See the [crate-level documentation][crate] for more details.
#[proc_macro]
pub fn include_configs(args: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as IncludeConfigsArgs);
    let path = match args {
        IncludeConfigsArgs::Path(p) => p.value(),
        IncludeConfigsArgs::PathEnv(env) => {
            let Ok(path) = std::env::var(env.value()) else {
                return compiler_error(
                    &env,
                    format!("environment variable `{}` not set", env.value()),
                );
            };
            path
        }
        IncludeConfigsArgs::PathEnvFallback(env, fallback) => {
            std::env::var(env.value()).unwrap_or_else(|_| fallback.value())
        }
    };

    let root = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let cfg_path = std::path::Path::new(&root).join(&path);

    let Ok(config_toml) = std::fs::read_to_string(&cfg_path) else {
        return compiler_error(path, format!("failed to read config file: {:?}", cfg_path));
    };

    quote! {
        ::axconfig_macros::parse_configs!(#config_toml);
    }
    .into()
}

enum IncludeConfigsArgs {
    Path(LitStr),
    PathEnv(LitStr),
    PathEnvFallback(LitStr, LitStr),
}

impl Parse for IncludeConfigsArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        if input.peek(LitStr) {
            return Ok(IncludeConfigsArgs::Path(input.parse()?));
        }

        let mut env = None;
        let mut fallback = None;
        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let str: LitStr = input.parse()?;

            match ident.to_string().as_str() {
                "path_env" => {
                    if env.is_some() {
                        return Err(Error::new(ident.span(), "duplicate parameter `path_env`"));
                    }
                    env = Some(str);
                }
                "fallback" => {
                    if fallback.is_some() {
                        return Err(Error::new(ident.span(), "duplicate parameter `fallback`"));
                    }
                    fallback = Some(str);
                }
                _ => {
                    return Err(Error::new(
                        ident.span(),
                        format!("unexpected parameter `{}`", ident),
                    ))
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        match (env, fallback) {
            (Some(env), None) => Ok(IncludeConfigsArgs::PathEnv(env)),
            (Some(env), Some(fallback)) => Ok(IncludeConfigsArgs::PathEnvFallback(env, fallback)),
            _ => Err(Error::new(
                input.span(),
                "missing required parameter `path_env`",
            )),
        }
    }
}
