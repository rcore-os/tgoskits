//! Call through the definition-owned macro associated with a trait path.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    Expr, Path, Result, Token, parenthesized,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

pub struct Call {
    interface: Path,
    method: syn::Ident,
    arguments: Punctuated<Expr, Token![,]>,
}

impl Parse for Call {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut interface: Path = input.parse()?;
        if interface.segments.len() < 2
            || interface
                .segments
                .iter()
                .any(|part| !part.arguments.is_empty())
        {
            return Err(syn::Error::new_spanned(
                interface,
                "expected an interface method path such as Clock::ticks",
            ));
        }
        let method = interface
            .segments
            .pop()
            .expect("at least two path segments")
            .ident;
        interface.segments.pop_punct();
        let arguments = if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            input.parse_terminated(Expr::parse, Token![,])?
        } else if input.peek(syn::token::Paren) {
            let content;
            parenthesized!(content in input);
            content.parse_terminated(Expr::parse, Token![,])?
        } else {
            Punctuated::new()
        };
        Ok(Self {
            interface,
            method,
            arguments,
        })
    }
}

pub fn expand(call: Call) -> TokenStream {
    let Call {
        interface,
        method,
        arguments,
    } = call;
    // Do not insert an unsafe block: the generated caller carries the method's
    // safety contract, which must also hold when called through this macro.
    quote!(#interface!(@call #method (#arguments)))
}
