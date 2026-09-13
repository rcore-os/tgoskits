//! Validate the supported function contract and give exported shims named types.

use std::collections::BTreeSet;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    FnArg, Lifetime, Result, ReturnType, Signature, Type, parse_quote, visit::Visit,
    visit_mut::VisitMut,
};

pub fn validate(sig: &Signature) -> Result<()> {
    if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &sig.generics,
            "trait-ffi methods cannot have generic parameters or where clauses",
        ));
    }
    if sig.constness.is_some()
        || sig.asyncness.is_some()
        || sig.abi.is_some()
        || sig.variadic.is_some()
    {
        return Err(syn::Error::new_spanned(
            sig,
            "trait-ffi requires synchronous non-const, non-variadic methods; select ABI on the \
             trait",
        ));
    }
    for input in &sig.inputs {
        if let FnArg::Receiver(_) = input {
            return Err(syn::Error::new_spanned(
                input,
                "trait-ffi methods cannot have a self receiver",
            ));
        }
    }
    let mut validator = TypeValidator { error: None };
    validator.visit_signature(sig);
    validator.error.map_or(Ok(()), Err)
}

struct TypeValidator {
    error: Option<syn::Error>,
}

impl<'ast> Visit<'ast> for TypeValidator {
    fn visit_type(&mut self, ty: &'ast Type) {
        if matches!(ty, Type::ImplTrait(_) | Type::Macro(_) | Type::Infer(_)) {
            self.error = Some(syn::Error::new_spanned(
                ty,
                "trait-ffi requires concrete types, not impl Trait, type macros or inferred types",
            ));
        }
        syn::visit::visit_type(self, ty);
    }

    fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
        if segment.ident == "Self" {
            self.error = Some(syn::Error::new_spanned(
                segment,
                "Self is not supported in interface signatures; use a concrete type",
            ));
        }
        syn::visit::visit_path_segment(self, segment);
    }
}

pub struct ExportSignature {
    pub aliases: TokenStream,
    pub inputs: Vec<TokenStream>,
    pub output: TokenStream,
    pub lifetimes: Vec<Lifetime>,
    pub arguments: Vec<syn::Ident>,
}

/// Type aliases live beside the interface, so an implementation does not need
/// to import private names used by default methods. Elided borrow lifetimes are
/// made explicit before moving those types into aliases.
pub fn export_signature(sig: &Signature, path: &TokenStream) -> Result<ExportSignature> {
    let mut aliases = TokenStream::new();
    let mut inputs = Vec::new();
    let mut arguments = Vec::new();
    let mut borrows = BorrowLifetimes::default();
    for (index, input) in sig.inputs.iter().enumerate() {
        let FnArg::Typed(input) = input else {
            unreachable!("validated associated function")
        };
        let mut ty = (*input.ty).clone();
        borrows.visit_type_mut(&mut ty);
        let name = format_ident!("Arg{index}");
        let argument = format_ident!("__arg{index}");
        let (alias, usage) = type_alias(&name, &ty, path);
        aliases.extend(alias);
        inputs.push(quote!(#argument: #usage));
        arguments.push(argument);
    }
    let input_lifetimes = borrows.names.clone();
    borrows.output = true;
    borrows.return_lifetime = if input_lifetimes.len() == 1 {
        input_lifetimes.iter().next().cloned()
    } else {
        None
    };
    let output = match &sig.output {
        ReturnType::Default => quote!(),
        ReturnType::Type(_, ty) if matches!(**ty, Type::Never(_)) => quote!(-> !),
        ReturnType::Type(_, ty) => {
            let mut ty = (**ty).clone();
            borrows.visit_type_mut(&mut ty);
            if borrows.ambiguous_return {
                return Err(syn::Error::new_spanned(
                    &sig.output,
                    "borrowed return requires exactly one input lifetime or an explicit 'static \
                     lifetime",
                ));
            }
            let (alias, usage) = type_alias(&format_ident!("Output"), &ty, path);
            aliases.extend(alias);
            quote!(-> #usage)
        }
    };
    let lifetimes = borrows
        .names
        .iter()
        .filter(|name| name.as_str() != "'static")
        .map(|name| Lifetime::new(name, sig.ident.span()))
        .collect();
    Ok(ExportSignature {
        aliases,
        inputs,
        output,
        lifetimes,
        arguments,
    })
}

fn type_alias(name: &syn::Ident, ty: &Type, path: &TokenStream) -> (TokenStream, TokenStream) {
    let mut collector = BorrowLifetimes::default();
    let mut ty = ty.clone();
    collector.visit_type_mut(&mut ty);
    let lifetimes: Vec<_> = collector
        .names
        .iter()
        .filter(|name| name.as_str() != "'static")
        .map(|name| Lifetime::new(name, proc_macro2::Span::call_site()))
        .collect();
    let generics = if lifetimes.is_empty() {
        quote!()
    } else {
        quote!(<#(#lifetimes),*>)
    };
    DefinitionPaths { depth: 2 }.visit_type_mut(&mut ty);
    (
        quote!(pub type #name #generics = #ty;),
        quote!(#path::#name #generics),
    )
}

#[derive(Default)]
struct BorrowLifetimes {
    names: BTreeSet<String>,
    next: usize,
    output: bool,
    return_lifetime: Option<String>,
    ambiguous_return: bool,
}

impl BorrowLifetimes {
    fn resolve(&mut self, lifetime: Option<&Lifetime>) -> Lifetime {
        let name = match lifetime {
            Some(lifetime) if lifetime.ident != "_" => lifetime.to_string(),
            _ if self.output => self.return_lifetime.clone().unwrap_or_else(|| {
                self.ambiguous_return = true;
                "'static".into()
            }),
            _ => {
                let name = format!("'__trait_ffi_{}", self.next);
                self.next += 1;
                name
            }
        };
        self.names.insert(name.clone());
        Lifetime::new(&name, proc_macro2::Span::call_site())
    }
}

impl VisitMut for BorrowLifetimes {
    fn visit_type_reference_mut(&mut self, reference: &mut syn::TypeReference) {
        reference.lifetime = Some(self.resolve(reference.lifetime.as_ref()));
        self.visit_type_mut(&mut reference.elem);
    }
    fn visit_lifetime_mut(&mut self, lifetime: &mut Lifetime) {
        *lifetime = self.resolve(Some(lifetime));
    }
    // Bare function and HRTB lifetimes have their own binder. They remain
    // within the alias and must not become parameters of the exported shim.
    fn visit_type_fn_ptr_mut(&mut self, _ty: &mut syn::TypeFnPtr) {}
    fn visit_trait_bound_mut(&mut self, bound: &mut syn::TraitBound) {
        if bound.lifetimes.is_none() {
            syn::visit_mut::visit_trait_bound_mut(self, bound);
        }
    }
}

pub fn caller_signature(sig: &Signature) -> Signature {
    named_signature(sig, 1)
}

pub fn named_signature(sig: &Signature, depth: usize) -> Signature {
    let mut sig = sig.clone();
    for (index, input) in sig.inputs.iter_mut().enumerate() {
        if let FnArg::Typed(input) = input {
            let name = format_ident!("__arg{index}");
            *input.pat = parse_quote!(#name);
        }
    }
    if depth != 0 {
        DefinitionPaths { depth }.visit_signature_mut(&mut sig);
    }
    sig
}

/// Moving a signature into a generated module must not change what its
/// relative type paths or array-length constant paths refer to.
struct DefinitionPaths {
    depth: usize,
}

impl VisitMut for DefinitionPaths {
    fn visit_path_mut(&mut self, path: &mut syn::Path) {
        syn::visit_mut::visit_path_mut(self, path);
        let Some(first) = path.segments.first() else {
            return;
        };
        if path.leading_colon.is_some() || (first.ident != "self" && first.ident != "super") {
            return;
        }
        let skip_self = usize::from(first.ident == "self");
        let segments = core::mem::take(&mut path.segments);
        path.segments = (0..self.depth)
            .map(|_| parse_quote!(super))
            .chain(segments.into_iter().skip(skip_self))
            .collect();
    }
}
