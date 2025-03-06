use proc_macro::TokenStream as TokenStream1;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{FnArg, Ident, Path, spanned::Spanned};

mod items;

use items::{ApiModImplItem, ApiModItem, ItemApiMod, ItemApiModImpl, ItemApiModList};

/// Find the path to the `axvisor_api` crate.
fn find_axvisor_api_crate() -> TokenStream {
    match crate_name("axvisor_api") {
        Ok(FoundCrate::Itself) => quote! { crate },
        Ok(FoundCrate::Name(name)) => {
            let name = Ident::new(&name, Span::call_site());
            quote! { #name }
        }
        Err(_) => quote! { compile_error!("`axvisor_api` crate not found") },
    }
}

/// Capitalize the first letter of a string.
///
/// From: `https://stackoverflow.com/questions/38406793/why-is-capitalizing-the-first-letter-of-a-string-so-convoluted-in-rust`
fn capitalize_first_letter(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn get_api_trait_name(module_name: impl AsRef<str>, span: Span) -> Ident {
    let module_name = module_name.as_ref();
    let trait_name = format!("{}ApiTrait", capitalize_first_letter(module_name));
    Ident::new(&trait_name, span)
}

fn process_api_mod(module: ItemApiMod, axvisor_api_path: &TokenStream) -> TokenStream {
    let attrs = &module.attrs;
    let vis = &module.vis;
    let mod_token = &module.mod_token;
    let mod_ident = &module.ident;

    // Split the items into regular items and API functions
    let mut regular_items = vec![];
    let mut api_fn_items = vec![];

    for item in &module.items {
        match item {
            ApiModItem::Regular(item) => regular_items.push(item),
            ApiModItem::ApiFnDef(item) => api_fn_items.push(item),
        }
    }

    if api_fn_items.is_empty() {
        return quote! {
            #(#attrs)*
            #vis #mod_token #mod_ident {
                #(#regular_items)*
            }
        };
    }

    // Generate the API trait
    let trait_ident = get_api_trait_name(&mod_ident.to_string(), mod_ident.span());
    let api_fn_attrs = api_fn_items
        .iter()
        .map(|item| &item.attrs)
        .collect::<Vec<_>>();
    let api_fn_signatures = api_fn_items
        .iter()
        .map(|item| &item.sig)
        .collect::<Vec<_>>();

    let trait_def = quote! {
        #[doc(hidden)]
        #[#axvisor_api_path::__priv::crate_interface::def_interface]
        pub trait #trait_ident {
            #(#(#api_fn_attrs)* #api_fn_signatures;)*
        }
    };

    // Generate the API function implementations
    let mut api_fn_impls = quote! {};
    for api_fn_item in api_fn_items {
        let attrs = &api_fn_item.attrs;
        let sig = &api_fn_item.sig;
        let fn_name = &sig.ident;
        let args = &sig
            .inputs
            .iter()
            .map(|arg| match arg {
                FnArg::Receiver(_) => panic!("API functions cannot have self arguments"),
                FnArg::Typed(pat) => &pat.pat,
            })
            .collect::<Vec<_>>();

        api_fn_impls.extend(quote! {
            #(#attrs)*
            pub #sig {
                #axvisor_api_path::__priv::crate_interface::call_interface!(
                    #trait_ident::#fn_name, #(#args),*
                )
            }
        });
    }

    quote! {
        #(#attrs)*
        #vis #mod_token #mod_ident {
            #(#regular_items)*

            #api_fn_impls

            #trait_def
        }
    }
}

fn process_api_mods(module: ItemApiModList) -> TokenStream {
    let mut output = TokenStream::new();
    let axvisor_api_path = find_axvisor_api_crate();

    for module in module.items {
        output.extend(process_api_mod(module, &axvisor_api_path));
    }

    output
}

fn get_implementee_reuse_ident(implementee: &Path) -> Ident {
    let mut ident = String::from(if implementee.leading_colon.is_some() {
        "__axvisor_api_implementee_abs"
    } else {
        "__axvisor_api_implementee_rel"
    });

    for seg in implementee.segments.iter() {
        ident.push('_');
        ident.push_str(seg.ident.to_string().as_str());
    }

    Ident::new(&ident, implementee.span())
}

fn process_api_mod_impl(implementee: Path, input: ItemApiModImpl) -> TokenStream {
    let attrs = &input.attrs;
    let vis = &input.vis;
    let mod_token = &input.mod_token;
    let mod_ident = &input.ident;

    let implementee_name = match implementee.segments.last() {
        Some(segment) => segment.ident.to_string(),
        None => return quote! { compile_error!("Invalid implementee path") },
    };
    let implementee_trait_ident = get_api_trait_name(&implementee_name, implementee.span());
    // we should reuse the implementee mod path besides the implementing mod, to make sure the `impl` block can find
    // the corrent trait.
    let implementee_reuse_ident = get_implementee_reuse_ident(&implementee);

    let axvisor_api_path = find_axvisor_api_crate();

    let mut regular_items = vec![];
    let mut api_fn_items = vec![];
    for item in input.items {
        match item {
            ApiModImplItem::Regular(item) => regular_items.push(item),
            ApiModImplItem::ApiFnImpl(item) => api_fn_items.push(item),
        }
    }

    let mut api_fn_impls = TokenStream::new();
    for api_fn_item in api_fn_items {
        let attrs = &api_fn_item.attrs;
        let sig = &api_fn_item.sig;
        let body = &api_fn_item.block;

        api_fn_impls.extend(quote! {
            #(#attrs)*
            #sig #body
        });
    }

    quote! {
        #[doc(hidden)]
        use #implementee as #implementee_reuse_ident;

        #(#attrs)*
        #vis #mod_token #mod_ident {
            #(#regular_items)*

            #[doc(hidden)]
            pub struct __Impl;
            #[#axvisor_api_path::__priv::crate_interface::impl_interface]
            impl super::#implementee_reuse_ident::#implementee_trait_ident for __Impl {
                #api_fn_impls
            }
        }
    }
}

#[proc_macro]
pub fn api_mod(input: TokenStream1) -> TokenStream1 {
    process_api_mods(syn::parse_macro_input!(input as ItemApiModList)).into()
}

#[proc_macro_attribute]
pub fn api_mod_impl(attr: TokenStream1, input: TokenStream1) -> TokenStream1 {
    process_api_mod_impl(
        syn::parse_macro_input!(attr as Path),
        syn::parse_macro_input!(input as ItemApiModImpl),
    )
    .into()
}
