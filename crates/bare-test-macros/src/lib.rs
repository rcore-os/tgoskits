extern crate proc_macro;
#[macro_use]
extern crate quote;
extern crate core;
extern crate proc_macro2;
extern crate syn;

use proc_macro::TokenStream;
use syn::{Item, ItemMod, parse, spanned::Spanned};

#[proc_macro]
pub fn build_test_setup(_input: TokenStream) -> TokenStream {
    quote! {
    println!("cargo::rustc-link-arg-tests=-Tlink-test.x");
    println!("cargo::rustc-link-arg-tests=-znostart-stop-gc");
    }
    .into()
}

#[proc_macro_attribute]
pub fn tests(args: TokenStream, input: TokenStream) -> TokenStream {
    match tests_impl(args, input) {
        Ok(tokens) => tokens,
        Err(e) => e.to_compile_error().into(),
    }
}

fn tests_impl(_args: TokenStream, input: TokenStream) -> Result<TokenStream, parse::Error> {
    let krate = format_ident!("bare_test");
    let module: ItemMod = syn::parse(input)?;

    let ident = &module.ident;

    let mut untouched_tokens = vec![];
    let mut test_functions = vec![];
    let span = module.span();

    let items = &module
        .content
        .ok_or(parse::Error::new(
            span,
            "module must be inline (e.g. `mod foo {}`)",
        ))?
        .1;

    for item in items {
        match item {
            Item::Fn(f) if f.attrs.iter().any(|attr| attr.path().is_ident("test")) => {
                let f_name = &f.sig.ident;
                let _f_name = format_ident!("__{}", f.sig.ident);
                let block = &f.block;
                let static_name = format_ident!("__TEST_{}", f_name.to_string().to_uppercase());
                let f_name_str = f_name.to_string();
                let f_atters = &f.attrs;

                let mut timeout = 0u64;
                if let Some(attr) = f_atters.iter().find(|attr| attr.path().is_ident("timeout")) {
                    if let syn::Meta::NameValue(nv) = &attr.meta
                        && let syn::Expr::Lit(lit_int) = &nv.value
                        && let syn::Lit::Int(int_lit) = &lit_int.lit
                    {
                        timeout = int_lit.base10_parse::<u64>()?;
                    } else {
                        return Err(syn::Error::new(attr.span(), "invalid timeout attribute"));
                    }
                }

                test_functions.push(quote! {
                    #(#f_atters)*
                    fn #f_name() {
                        #_f_name()
                    }

                    fn #_f_name() {
                        #block
                    }

                    #[used(linker)]
                    #[unsafe(link_section = ".test_case")]
                    static #static_name: #krate::TestCase = #krate::TestCase {
                        name: #f_name_str,
                        timeout_ms: #timeout,
                        test_fn: #_f_name,
                    };
                });
            }
            _ => {
                untouched_tokens.push(item);
            }
        }
    }

    Ok(quote!(
        #[cfg(test)]
        mod #ident{
            #(#untouched_tokens)*
            #(#test_functions)*

        }
    )
    .into())
}
