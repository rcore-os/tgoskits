//! Procedural macros for the `axtest` runtime.
//!
//! This crate provides two attribute macros:
//! - [`def_mod`] for declaring test modules with optional init/exit hooks.
//! - [`axtest`] / [`def_test`] for registering test functions into the
//!   linker-collected `.axtest_array` section.
//!
//! The generated descriptors are consumed by `axtest::init().run_tests()` at
//! runtime.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Error, Ident, Item, ItemFn, ItemMod, LitStr, Meta, Token, punctuated::Punctuated};

/// Define a test module that is only compiled under `cfg(axtest)`.
///
/// This macro should be applied to an inline module that may optionally define:
/// - `fn axtest_init(sym: axtest::AxTestDescriptor)`
/// - `fn axtest_exit(sym: axtest::AxTestDescriptor)`
///
/// If present, these hooks are invoked around each `#[def_test]` execution in
/// the same module.
///
/// # Arguments
///
/// This attribute accepts no arguments.
///
/// # Errors
///
/// Emits a compile error if any attribute argument is provided.
///
/// # Example
///
/// ```rust,ignore
/// use axtest_macros::{def_mod, def_test};
///
/// #[def_mod]
/// mod sample {
///     fn axtest_init(_sym: axtest::AxTestDescriptor) {
///         // per-test setup
///     }
///
///     fn axtest_exit(_sym: axtest::AxTestDescriptor) {
///         // per-test teardown
///     }
///
///     #[def_test]
///     fn smoke() {
///         // test body
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn def_mod(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return Error::new(
            proc_macro2::Span::call_site(),
            "def_mod does not accept arguments",
        )
        .to_compile_error()
        .into();
    }

    let mut item = syn::parse_macro_input!(item as ItemMod);

    let (has_init, has_exit) = if let Some((_, items)) = &item.content {
        let mut has_init = false;
        let mut has_exit = false;
        for it in items {
            if let Item::Fn(func) = it {
                if func.sig.ident == "axtest_init" {
                    has_init = true;
                } else if func.sig.ident == "axtest_exit" {
                    has_exit = true;
                }
            }
        }
        (has_init, has_exit)
    } else {
        (false, false)
    };

    let init_hook = if has_init {
        quote! { Some(axtest_init as fn(axtest::AxTestDescriptor)) }
    } else {
        quote! { None }
    };
    let exit_hook = if has_exit {
        quote! { Some(axtest_exit as fn(axtest::AxTestDescriptor)) }
    } else {
        quote! { None }
    };

    if let Some((_, items)) = &mut item.content {
        items.push(syn::parse_quote! {
            #[used]
            #[unsafe(link_section = ".axtest_mod_hooks")]
            #[allow(non_upper_case_globals)]
            static __axtest_mod_hooks: axtest::AxTestModHookDescriptor =
                axtest::AxTestModHookDescriptor::new(module_path!(), #init_hook, #exit_hook);
        });
    }

    let expanded = quote! {
        #[cfg(axtest)]
        #item
    };
    expanded.into()
}

/// Define and register a test case for the `axtest` runtime.
///
/// This macro expands a function into:
/// - a static [`axtest::AxTestDescriptor`] placed in linker section `.axtest`,
/// - a wrapped test function that optionally calls module-level init/exit hooks.
///
/// The function can either:
/// - return `axtest::AxTestResult`, or
/// - return nothing, in which case `AxTestResult::Ok` is appended automatically.
///
/// Existing attributes on the function are preserved. Special handling:
/// - `#[ignore]` or `#[ignore = "reason"]` marks the case as ignored.
/// - `#[should_panic]` marks the case as expected-failure.
///
/// # Arguments
///
/// Supported macro arguments:
/// - `standard`: force standard execution mode.
/// - `custom = "name"`: bind this case to a named custom executor.
///
/// Without arguments, mode defaults to `standard` unless `#[ignore]` is set.
///
/// # Errors
///
/// Emits a compile error when:
/// - an unsupported argument is used,
/// - `custom` is provided without a string literal.
///
/// # Examples
///
/// Basic test:
/// ```rust,ignore
/// use axtest_macros::def_test;
///
/// #[def_test]
/// fn simple_case() {
///     // no explicit return needed
/// }
/// ```
///
/// Custom executor test:
/// ```rust,ignore
/// use axtest_macros::def_test;
///
/// #[def_test(custom = "thread")]
/// fn thread_case() -> axtest::AxTestResult {
///     axtest::AxTestResult::Ok
/// }
/// ```
///
/// Ignored test with reason:
/// ```rust,ignore
/// use axtest_macros::def_test;
///
/// #[def_test]
/// #[ignore = "depends on external device"]
/// fn hw_dependent_case() {}
/// ```
#[proc_macro_attribute]
pub fn axtest(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_axtest(attr, item)
}

#[proc_macro_attribute]
pub fn def_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_axtest(attr, item)
}

fn expand_axtest(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = syn::parse_macro_input!(item as ItemFn);
    let args = syn::parse_macro_input!(attr with Punctuated::<Meta, Token![,]>::parse_terminated);

    let fn_name = &item.sig.ident;
    let fn_attrs = &item.attrs;
    let fn_stmts = &item.block.stmts;

    // Analyzing the `#[ignore]` and `#[should_panic]` attributes
    let mut should_panic = false;
    let mut ignore = false;
    let mut ignore_reason: Option<String> = None;

    for attr in &item.attrs {
        // Check #[ignore = "reason"] attribute
        if attr.path().is_ident("ignore") {
            ignore = true;
            if let syn::Meta::NameValue(nv) = &attr.meta
                && let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
            {
                ignore_reason = Some(s.value());
            }
        }
        // Check #[should_panic(expected = "...")]
        if attr.path().is_ident("should_panic") {
            should_panic = true;
            if let syn::Meta::NameValue(nv) = &attr.meta
                && let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(_),
                    ..
                }) = &nv.value
            {
                // should_panic_expected = s.value();
            }
        }
    }

    // Check if function returns TestResult
    let has_return_type = !matches!(item.sig.output, syn::ReturnType::Default);

    let mut execution_mode = if ignore {
        quote!(axtest::AxTestExecutionMode::Ignore)
    } else {
        quote!(axtest::AxTestExecutionMode::Standard)
    };
    let mut executor_name = String::new();

    for arg in args {
        match arg {
            Meta::Path(path) if path.is_ident("standard") => {
                execution_mode = quote!(axtest::AxTestExecutionMode::Standard);
            }
            Meta::NameValue(nv) if nv.path.is_ident("custom") => {
                let lit = match nv.value {
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) => s,
                    _ => {
                        return Error::new_spanned(nv, "`custom` expects a string literal")
                            .to_compile_error()
                            .into();
                    }
                };
                executor_name = lit.value();
                execution_mode = quote!(axtest::AxTestExecutionMode::Custom);
            }
            other => {
                return Error::new_spanned(
                    other,
                    "unsupported axtest argument, expected `standard` or `custom = \"name\"`",
                )
                .to_compile_error()
                .into();
            }
        }
    }

    let ignore_reason_lit = ignore_reason.unwrap_or_else(|| "ignored".to_string());

    // Generate a unique static name for the test descriptor
    let static_name = Ident::new(&format!("__axtest_descriptor_{}", fn_name), fn_name.span());

    let fn_name_str = fn_name.to_string();
    let executor_name_lit = LitStr::new(&executor_name, proc_macro2::Span::call_site());
    let expanded = if has_return_type {
        quote! {
            #[used]
            #[cfg(axtest)]
            #[unsafe(link_section = ".axtest_array")]
            #[allow(non_upper_case_globals)]
            static #static_name: axtest::AxTestDescriptor = axtest::AxTestDescriptor::new(
                #fn_name_str,
                module_path!(),
                #fn_name,
                #executor_name_lit,
                #should_panic,
                #ignore_reason_lit,
                #execution_mode,
            );

            #(#fn_attrs)*
            fn #fn_name() -> axtest::AxTestResult {
                let __axtest_desc = #static_name;
                axtest::call_module_init(module_path!(), __axtest_desc);
                let __axtest_result = (|| -> axtest::AxTestResult {
                    #(#fn_stmts)*
                })();
                axtest::call_module_exit(module_path!(), __axtest_desc);
                __axtest_result
            }
        }
    } else {
        quote! {
            #[used]
            #[cfg(axtest)]
            #[unsafe(link_section = ".axtest_array")]
            #[allow(non_upper_case_globals)]
            static #static_name: axtest::AxTestDescriptor = axtest::AxTestDescriptor::new(
                #fn_name_str,
                module_path!(),
                #fn_name,
                #executor_name_lit,
                #should_panic,
                #ignore_reason_lit,
                #execution_mode,
            );

            #(#fn_attrs)*
            fn #fn_name() -> axtest::AxTestResult {
                let __axtest_desc = #static_name;
                axtest::call_module_init(module_path!(), __axtest_desc);
                let __axtest_result = (|| -> axtest::AxTestResult {
                    #(#fn_stmts)*
                    axtest::AxTestResult::Ok
                })();
                axtest::call_module_exit(module_path!(), __axtest_desc);
                __axtest_result
            }
        }
    };

    expanded.into()
}
