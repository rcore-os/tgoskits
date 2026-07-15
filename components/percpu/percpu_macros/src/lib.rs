//! Macros to define and access a per-CPU data structure.
//!
//! **DO NOT** use this crate directly. Use the [ax-percpu] crate instead.
//!
//! [ax-percpu]: https://docs.rs/ax-percpu
//!
//! ## Implementation details of the `def_percpu` macro
//!
//! ### Core idea
//!
//! The core idea is to collect uninitialized storage for all per-CPU static
//! variables in the `.percpu.storage` input section. A separate initializer table describes
//! how to construct each value at its final runtime address after image
//! relocation. This is required for Rust values whose bytes cannot legally be
//! duplicated into another allocation.
//!
//! The address of a per-CPU static variable on a given CPU can be calculated by adding the offset of the variable
//! (relative to the section base) to the base address of the per-CPU data area on the CPU.
//!
//! ### How to access the per-CPU data
//!
//! To access a per-CPU static variable on a given CPU, three values are needed:
//!
//! - The runtime base of the current CPU's data area,
//!   - which is read through the architecture capability owned by `ax-cpu-local`.
//! - The offset of the per-CPU static variable relative to the per-CPU data area base,
//!   - calculated by ordinary Rust integer arithmetic from the linked template base.
//! - The size of the per-CPU static variable,
//!   - which we actually do not need to know, just give the right type to rust compiler.
//!
//! ### Generated code
//!
//! For each static variable `X` with type `T` that is defined with the `def_percpu` macro, the following items are
//! generated:
//!
//! - A `MaybeUninit` static variable `__PERCPU_X` in `.percpu.storage` that
//!   reserves the per-CPU storage. Primitive values use their matching atomic representation so
//!   hard-IRQ re-entry does not make safe reads and writes data-racy; objects
//!   retain `T` directly.
//!
//!   This variable is placed in the `.percpu` section. All attributes of the original static variable, as well as the
//!   initialization expression, are preserved. The expression is retained as
//!   a Rust `const` and instantiated independently in every runtime CPU area.
//!
//!   This variable is never, and should never be, accessed directly. To access the per-CPU data, the offset of the
//!   variable is, and should be, used.
//!
//! - A typed initializer registration in `.ax_percpu.init`. Its descriptor
//!   thunk is consumed only after the final image relocation and resolves the
//!   storage symbol to a checked template-relative scalar offset. Rust cannot
//!   encode this subtraction directly in a static integer: separate statics
//!   are distinct const-eval allocations, even when the linker later places
//!   them in one output section.
//!
//! - A zero-sized descriptor type `X_WRAPPER` that selects the appropriate
//!   object or primitive access surface from `ax-percpu`.
//!
//! - A static variable `X` of type `X_WRAPPER` that is used to access the per-CPU data.
//!
//!   This variable is always generated with the same visibility and attributes as the original static variable.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{Attribute, Error, ItemStatic};

#[cfg_attr(feature = "sp-naive", path = "naive.rs")]
mod address;

fn compiler_error(err: Error) -> TokenStream {
    err.to_compile_error().into()
}

fn def_percpu_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return compiler_error(Error::new(
            Span::call_site(),
            "expect an empty attribute: `#[def_percpu]`",
        ));
    }

    let ast = syn::parse_macro_input!(item as ItemStatic);

    let attrs = &ast.attrs;
    let vis = &ast.vis;
    let name = &ast.ident;
    let ty = &ast.ty;
    let init_expr = &ast.expr;

    let inner_symbol_name = &format_ident!("__PERCPU_{}", name);
    let alignment_descriptor_name = &format_ident!("__PERCPU_{}_ALIGNMENT", name);
    let initial_value_name = &format_ident!("__PERCPU_{}_INITIAL_VALUE", name);
    let initializer_name = &format_ident!("__PERCPU_{}_INITIALIZE", name);
    let descriptor_name = &format_ident!("__PERCPU_{}_DESCRIBE", name);
    let registration_name = &format_ident!("__PERCPU_{}_REGISTRATION", name);
    let symbol_provider_name = &format_ident!("__PERCPU_{}_SYMBOL", name);
    let struct_name = &format_ident!("{}_WRAPPER", name);
    let conditional_attrs = conditional_attributes(attrs);

    let ty_str = quote!(#ty).to_string();
    let is_primitive_int = ["bool", "u8", "u16", "u32", "u64", "usize"].contains(&ty_str.as_str());

    let (access_kind, storage_type, initial_value) = if is_primitive_int {
        let atomic_ty = match ty_str.as_str() {
            "bool" => quote!(::core::sync::atomic::AtomicBool),
            "u8" => quote!(::core::sync::atomic::AtomicU8),
            "u16" => quote!(::core::sync::atomic::AtomicU16),
            "u32" => quote!(::core::sync::atomic::AtomicU32),
            "u64" => quote!(::core::sync::atomic::AtomicU64),
            "usize" => quote!(::core::sync::atomic::AtomicUsize),
            _ => unreachable!("primitive type classification must stay exhaustive"),
        };
        (
            quote!(ax_percpu::PrimitiveAccess),
            atomic_ty.clone(),
            quote!(#atomic_ty::new(#init_expr)),
        )
    } else {
        (
            quote!(ax_percpu::ObjectAccess),
            quote!(#ty),
            quote!(#init_expr),
        )
    };

    let storage_definition = if cfg!(feature = "sp-naive") {
        quote! {
            static mut #inner_symbol_name: #storage_type = #initial_value;
        }
    } else {
        quote! {
            static mut #inner_symbol_name: ::core::mem::MaybeUninit<#storage_type> =
                ::core::mem::MaybeUninit::uninit();
        }
    };

    let offset = address::gen_offset(inner_symbol_name);
    let current_ptr = address::gen_current_ptr(inner_symbol_name, ty);
    let current_ptr_pinned =
        address::gen_current_ptr_pinned(inner_symbol_name, &format_ident!("pin"), ty);
    let remote_ptr = address::gen_remote_ptr(inner_symbol_name, &format_ident!("cpu_index"), ty);
    let initialization = if cfg!(feature = "sp-naive") {
        quote! {}
    } else {
        quote! {
            #(#conditional_attrs)*
            #[allow(non_upper_case_globals)]
            const #initial_value_name: #storage_type = #initial_value;

            #(#conditional_attrs)*
            #[allow(non_snake_case)]
            unsafe extern "C" fn #initializer_name(destination: *mut u8) {
                let destination = destination.cast::<::core::mem::MaybeUninit<#storage_type>>();
                // SAFETY: ax-percpu validates this record's offset, size, and
                // alignment against every exclusively owned runtime area before
                // invoking the typed initializer exactly once for that area.
                unsafe {
                    destination.write(::core::mem::MaybeUninit::new(#initial_value_name));
                }
            }

            #(#conditional_attrs)*
            #[allow(non_snake_case)]
            unsafe extern "C" fn #descriptor_name() -> ax_percpu::__priv::PerCpuInitDescriptor {
                let storage_address =
                    ::core::ptr::addr_of!(#inner_symbol_name).cast::<u8>() as usize;
                // SAFETY: every scalar is derived from this exact generated
                // MaybeUninit<Storage> object and its matching typed writer.
                unsafe {
                    ax_percpu::__priv::PerCpuInitDescriptor::new(
                        storage_address,
                        ::core::mem::size_of::<#storage_type>(),
                        ::core::mem::align_of::<#storage_type>(),
                        #initializer_name,
                    )
                }
            }

            #(#conditional_attrs)*
            #[cfg_attr(
                not(target_os = "macos"),
                unsafe(link_section = ".ax_percpu.init")
            )]
            #[used]
            static #registration_name: ax_percpu::__priv::PerCpuInitRegistration =
                // SAFETY: the generated descriptor thunk is immutable,
                // deterministic, final-image resident, and always describes
                // the same generated storage and initializer.
                unsafe { ax_percpu::__priv::PerCpuInitRegistration::new(#descriptor_name) };
        }
    };

    quote! {
        #[cfg_attr(
            not(target_os = "macos"),
            unsafe(link_section = ".ax_percpu.align")
        )]
        #[used]
        static #alignment_descriptor_name: usize = ::core::mem::align_of::<#storage_type>();

        #[cfg_attr(
            not(target_os = "macos"),
            unsafe(link_section = ".percpu.storage")
        )]
        #(#attrs)*
        #storage_definition

        #initialization

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #(#conditional_attrs)*
        #vis struct #symbol_provider_name;

        // SAFETY: every pointer is derived from this one typed template symbol
        // and the registered CPU-area layout.
        #(#conditional_attrs)*
        unsafe impl ax_percpu::__priv::PerCpuSymbol<#ty> for #symbol_provider_name {
            #[inline]
            fn offset() -> usize {
                #offset
            }

            #[inline]
            fn current_ptr(pin: &ax_percpu::BoundCpuPin<'_>) -> *const #ty {
                #current_ptr_pinned
            }

            #[inline]
            unsafe fn current_ptr_unchecked() -> *const #ty {
                unsafe { #current_ptr }
            }

            #[inline]
            fn remote_ptr(
                cpu_index: ax_percpu::CpuIndex,
            ) -> Result<*const #ty, ax_percpu::PerCpuError> {
                #remote_ptr
            }
        }

        #[doc = concat!("Wrapper type for the per-CPU data [`", stringify!(#name), "`]")]
        #[allow(non_camel_case_types)]
        #(#conditional_attrs)*
        #vis type #struct_name = ax_percpu::PerCpu<#ty, #symbol_provider_name, #access_kind>;

        #(#attrs)*
        #vis static #name: #struct_name = ax_percpu::PerCpu::new();
    }
    .into()
}

fn conditional_attributes(attrs: &[Attribute]) -> Vec<&Attribute> {
    attrs
        .iter()
        .filter(|attribute| {
            attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr")
        })
        .collect()
}
/// Defines a per-CPU static variable.
///
/// It should be used on a `static` variable definition.
///
/// See the documentation of the [ax-percpu](https://docs.rs/ax-percpu) crate for more details.
#[proc_macro_attribute]
pub fn def_percpu(attr: TokenStream, item: TokenStream) -> TokenStream {
    def_percpu_impl(attr, item)
}
