use syn::{
    Attribute, Ident, Item, Signature, Token, Visibility, braced,
    parse::{Parse, ParseStream},
    token::Brace,
};

/// An API function definition, defined with the `extern fn` syntax.
pub struct ItemApiFnDef {
    /// Attributes of the function.
    pub attrs: Vec<Attribute>,
    #[expect(dead_code)]
    /// The `extern` keyword.
    pub extern_token: Token![extern],
    /// The function signature.
    pub sig: Signature,
    #[expect(dead_code)]
    /// The semicolon at the end of the function.
    pub semi_token: Token![;],
}

impl Parse for ItemApiFnDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let extern_token = input.parse()?;
        let sig = input.parse()?;
        let semi_token = input.parse()?;

        Ok(Self {
            attrs,
            extern_token,
            sig,
            semi_token,
        })
    }
}

/// An item in a [`ItemApiMod`], which can be a regular [`Item`] or an [API function](`ItemApiFn`).
pub enum ApiModItem {
    /// A regular item.
    Regular(Item),
    /// An API function definition.
    ApiFnDef(ItemApiFnDef),
}

impl Parse for ApiModItem {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Attributes will be parsed twice, but it's not a big deal.
        let forked = input.fork();
        let _ = forked.call(Attribute::parse_outer)?;
        let is_api_fn = forked.peek(Token![extern]) && forked.peek2(Token![fn]);
        drop(forked);

        if is_api_fn {
            Ok(Self::ApiFnDef(input.parse()?))
        } else {
            Ok(Self::Regular(input.parse()?))
        }
    }
}

/// A module that contains API functions, used in [`api_mod!`)[crate::api_mod] macro.
///
/// API functions are defined with the `extern fn` syntax. An example of `ItemApiMod` is:
///
/// ```
/// api_mod! {
///     pub mod a {
///         extern fn a();
///         extern fn a2(arg: i32);
///     }
/// }
/// ```
pub struct ItemApiMod {
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
    pub items: Vec<ApiModItem>,
}

impl Parse for ItemApiMod {
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

/// A list of [`ItemApiMod`]s, which looks like this:
///
/// ```
/// api_mod! {
///     pub mod a {
///         extern fn a();
///     }
///
///     pub mod b {
///         extern fn b();
///     }
/// }
/// ```
pub struct ItemApiModList {
    /// The list of items
    pub items: Vec<ItemApiMod>,
}

impl Parse for ItemApiModList {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut items = vec![];

        while !input.is_empty() {
            items.push(input.parse()?);
        }

        Ok(Self { items })
    }
}

/// An API function implementation, defined with the `extern fn` syntax.
pub struct ItemApiFnImpl {
    pub attrs: Vec<Attribute>,
    #[expect(dead_code)]
    pub extern_token: Token![extern],
    pub sig: Signature,
    pub block: syn::Block,
}

impl Parse for ItemApiFnImpl {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let extern_token = input.parse()?;
        let sig = input.parse()?;
        let block = input.parse()?;

        Ok(Self {
            attrs,
            extern_token,
            sig,
            block,
        })
    }
}

/// An item in a [`ItemApiModImpl`], which can be a regular [`Item`] or an [API function](`ItemApiFnImpl`).
pub enum ApiModImplItem {
    Regular(Item),
    ApiFnImpl(ItemApiFnImpl),
}

impl Parse for ApiModImplItem {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let forked = input.fork();
        let _ = forked.call(Attribute::parse_outer)?;
        let is_api_fn = forked.peek(Token![extern]) && forked.peek2(Token![fn]);
        drop(forked);

        if is_api_fn {
            Ok(Self::ApiFnImpl(input.parse()?))
        } else {
            Ok(Self::Regular(input.parse()?))
        }
    }
}

pub struct ItemApiModImpl {
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
    pub items: Vec<ApiModImplItem>,
}

impl Parse for ItemApiModImpl {
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
