//! Definitions of custom items used in the `api_mod!` and `api_mod_impl!` macros.
//!
//! `api_mod!` and `api_mod_impl!` have very similar structures.

use syn::{
    Attribute, Block, Ident, Item, Signature, Token, Visibility, braced,
    parse::{Parse, ParseStream},
    token::Brace,
};

/// An API function, defined with the `extern fn` syntax. It represents both the definition and the implementation. For
/// the definition, `T` is `Token![;]`, and for the implementation, `T` is `syn::Block`.
pub struct ItemApiFn<T: Parse> {
    /// Attributes of the function.
    pub attrs: Vec<Attribute>,
    #[expect(dead_code)]
    /// The `extern` keyword.
    pub extern_token: Token![extern],
    /// The function signature.
    pub sig: Signature,
    /// The body of the function, or a semicolon.
    pub body: T,
}

impl<T: Parse> Parse for ItemApiFn<T> {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let extern_token = input.parse()?;
        let sig = input.parse()?;
        let body = input.parse()?;

        Ok(Self {
            attrs,
            extern_token,
            sig,
            body,
        })
    }
}

/// An item in a [`ItemApiMod`], which can be a regular [`Item`] or an [API function](`ItemApiFn`). As `ItemApiFn`, this
/// enum represents both the definition and the implementation.
pub enum ApiModItem<T: Parse> {
    Regular(Item),
    ApiFn(ItemApiFn<T>),
}

impl<T: Parse> Parse for ApiModItem<T> {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Attributes will be parsed twice, but it's not a big deal.
        let forked = input.fork();
        let _ = forked.call(Attribute::parse_outer)?;
        let is_api_fn = forked.peek(Token![extern]) && forked.peek2(Token![fn]);
        drop(forked);

        if is_api_fn {
            Ok(Self::ApiFn(input.parse()?))
        } else {
            Ok(Self::Regular(input.parse()?))
        }
    }
}

/// A module that contains the definition or implementation of API functions, and marked by `#[api_mod]` or
/// `#[api_mod_impl]`.
pub struct ItemApiMod<T: Parse> {
    /// Attributes of the module.
    pub attrs: Vec<Attribute>,
    /// Visibility of the module.
    pub vis: Visibility,
    /// The `mod` keyword.
    pub mod_token: Token![mod],
    /// The identifier of the module.
    pub ident: Ident,
    #[expect(dead_code)]
    /// The brace token.
    pub brace_token: Brace,
    /// The items in the module.
    pub items: Vec<ApiModItem<T>>,
}

impl<T: Parse> Parse for ItemApiMod<T> {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let vis = input.parse()?;
        let mod_token = input.parse()?;
        let ident = input.parse()?;

        let content;
        let brace_token = braced!(content in input);
        let mut items = vec![];

        while !content.is_empty() {
            items.push(content.parse()?);
        }

        Ok(Self {
            attrs,
            vis,
            mod_token,
            ident,
            brace_token,
            items,
        })
    }
}

/// A module that contains the definition of API functions, marked by `#[api_mod]`.
pub type ItemApiModDef = ItemApiMod<Token![;]>;
/// A module that contains the implementation of API functions, marked by `#[api_mod_impl]`.
pub type ItemApiModImpl = ItemApiMod<Block>;
