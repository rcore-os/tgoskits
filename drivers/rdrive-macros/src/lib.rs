use proc_macro::TokenStream;
use quote::{format_ident, quote};

#[proc_macro]
pub fn __mod_maker(input: TokenStream) -> TokenStream {
    let mut _mod = syn::parse_macro_input!(input as syn::ItemMod);
    let mut name = String::new();

    for c in _mod.content.as_ref().unwrap().1.iter() {
        if let syn::Item::Static(st) = c
            && let syn::Expr::Struct(expr_struct) = st.expr.as_ref()
        {
            for field in &expr_struct.fields {
                if let syn::Member::Named(ident) = &field.member
                    && *ident == "name"
                    && let syn::Expr::Group(expr_group) = &field.expr
                    && let syn::Expr::Lit(expr_lit) = expr_group.expr.as_ref()
                    && let syn::Lit::Str(lit_str) = &expr_lit.lit
                {
                    name = lit_str.value();
                }
            }
        }
    }

    name = rename(name.as_str());

    let mod_name = name.to_lowercase();

    _mod.ident = format_ident!("__mod_{}", mod_name);

    for c in _mod.content.as_mut().unwrap().1.iter_mut() {
        if let syn::Item::Static(st) = c {
            st.ident = format_ident!("__DRIVER_{}", name.to_uppercase());
        }
    }

    quote! { #_mod }.into()
}

fn rename(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_alphabetic() || c == '_' || (i > 0 && c.is_ascii_digit()) {
            result.push(c);
        } else {
            result.push('_');
        }
    }
    result
}
