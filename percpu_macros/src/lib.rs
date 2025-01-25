//! Macros to define and access a per-CPU data structure.
//!
//! **DO NOT** use this crate directly. Use the [percpu] crate instead.
//!
//! [percpu]: https://docs.rs/percpu
//!
//! ## Implementation details of the `def_percpu` macro
//!
//! ### Core idea
//!
//! The core idea is to collect all per-CPU static variables to a single section (i.e., `.percpu`), then allocate a
//! per-CPU data area, with the size equals to the size of the `.percpu` section, for each CPU (it can be done
//! statically or dynamically), then copy the `.percpu` section to each per-CPU data area during initialization.
//!
//! The address of a per-CPU static variable on a given CPU can be calculated by adding the offset of the variable
//! (relative to the section base) to the base address of the per-CPU data area on the CPU.
//!
//! ### How to access the per-CPU data
//!
//! To access a per-CPU static variable on a given CPU, three values are needed:
//!
//! - The base address of the per-CPU data area on the CPU,
//!   - which can be calculated by the base address of the whole per-CPU data area and the CPU ID,
//!   - and then stored in a register, like `TPIDR_EL1`/`TPIDR_EL2` on AArch64, or `gs` on x86_64.
//! - The offset of the per-CPU static variable relative to the per-CPU data area base,
//!   - which can be calculated by assembly notations, like `offset symbol` on x86_64, or `#:abs_g0_nc:symbol` on
//!     AArch64, or `%hi(symbol)` and `%lo(symbol)` on RISC-V.
//! - The size of the per-CPU static variable,
//!   - which we actually do not need to know, just give the right type to rust compiler.
//!
//! ### Generated code
//!
//! For each static variable `X` with type `T` that is defined with the `def_percpu` macro, the following items are
//! generated:
//!
//! - A static variable `__PERCPU_X` with type `T` that stores the per-CPU data.
//!
//!   This variable is placed in the `.percpu` section. All attributes of the original static variable, as well as the
//!   initialization expression, are preserved.
//!
//!   This variable is never, and should never be, accessed directly. To access the per-CPU data, the offset of the
//!   variable is, and should be, used.
//!
//! - A zero-sized wrapper struct `X_WRAPPER` that is used to access the per-CPU data.
//!
//!   Some methods are generated in this struct to access the per-CPU data. For primitive integer types, extra methods
//!   are generated to accelerate the access.
//!
//! - A static variable `X` of type `X_WRAPPER` that is used to access the per-CPU data.
//!
//!   This variable is always generated with the same visibility and attributes as the original static variable.
//!

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{Error, ItemStatic};

#[cfg_attr(feature = "sp-naive", path = "naive.rs")]
mod arch;

fn compiler_error(err: Error) -> TokenStream {
    err.to_compile_error().into()
}

/// Defines a per-CPU static variable.
///
/// It should be used on a `static` variable definition.
///
/// See the documentation of the [percpu](https://docs.rs/percpu) crate for more details.
#[proc_macro_attribute]
pub fn def_percpu(attr: TokenStream, item: TokenStream) -> TokenStream {
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
    let struct_name = &format_ident!("{}_WRAPPER", name);

    let ty_str = quote!(#ty).to_string();
    let is_primitive_int = ["bool", "u8", "u16", "u32", "u64", "usize"].contains(&ty_str.as_str());

    let no_preempt_guard = if cfg!(feature = "preempt") {
        quote! { let _guard = percpu::__priv::NoPreemptGuard::new(); }
    } else {
        quote! {}
    };

    // Do not generate `fn read_current()`, `fn write_current()`, etc for non primitive types.
    let read_write_methods = if is_primitive_int {
        let read_current_raw = arch::gen_read_current_raw(inner_symbol_name, ty);
        let write_current_raw =
            arch::gen_write_current_raw(inner_symbol_name, &format_ident!("val"), ty);

        quote! {
            /// Returns the value of the per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            /// Caller must ensure that preemption is disabled on the current CPU.
            #[inline]
            pub unsafe fn read_current_raw(&self) -> #ty {
                #read_current_raw
            }

            /// Set the value of the per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            /// Caller must ensure that preemption is disabled on the current CPU.
            #[inline]
            pub unsafe fn write_current_raw(&self, val: #ty) {
                #write_current_raw
            }

            /// Returns the value of the per-CPU static variable on the current CPU. Preemption will be disabled during
            /// the call.
            pub fn read_current(&self) -> #ty {
                #no_preempt_guard
                unsafe { self.read_current_raw() }
            }

            /// Set the value of the per-CPU static variable on the current CPU. Preemption will be disabled during the
            /// call.
            pub fn write_current(&self, val: #ty) {
                #no_preempt_guard
                unsafe { self.write_current_raw(val) }
            }
        }

        // Todo: maybe add `(read|write)_remote(_raw)?` here?
    } else {
        quote! {}
    };

    let offset = arch::gen_offset(inner_symbol_name);
    let current_ptr = arch::gen_current_ptr(inner_symbol_name, ty);
    quote! {
        #[cfg_attr(not(target_os = "macos"), link_section = ".percpu")] // unimplemented on macos
        #(#attrs)*
        static mut #inner_symbol_name: #ty = #init_expr;

        #[doc = concat!("Wrapper struct for the per-CPU data [`", stringify!(#name), "`]")]
        #[allow(non_camel_case_types)]
        #vis struct #struct_name {}

        #(#attrs)*
        #vis static #name: #struct_name = #struct_name {};

        impl #struct_name {
            /// Returns the offset relative to the per-CPU data area base.
            #[inline]
            pub fn offset(&self) -> usize {
                #offset
            }

            /// Returns the raw pointer of this per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            /// Caller must ensure that preemption is disabled on the current CPU.
            #[inline]
            pub unsafe fn current_ptr(&self) -> *const #ty {
                #current_ptr
            }

            /// Returns the reference of the per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            /// Caller must ensure that preemption is disabled on the current CPU.
            #[inline]
            pub unsafe fn current_ref_raw(&self) -> &#ty {
                &*self.current_ptr()
            }

            /// Returns the mutable reference of the per-CPU static variable on the current CPU.
            ///
            /// # Safety
            ///
            /// Caller must ensure that preemption is disabled on the current CPU.
            #[inline]
            #[allow(clippy::mut_from_ref)]
            pub unsafe fn current_ref_mut_raw(&self) -> &mut #ty {
                &mut *(self.current_ptr() as *mut #ty)
            }

            /// Manipulate the per-CPU data on the current CPU in the given closure.
            /// Preemption will be disabled during the call.
            pub fn with_current<F, T>(&self, f: F) -> T
            where
                F: FnOnce(&mut #ty) -> T,
            {
                #no_preempt_guard
                f(unsafe { self.current_ref_mut_raw() })
            }

            /// Returns the raw pointer of this per-CPU static variable on the given CPU.
            ///
            /// # Safety
            ///
            /// Caller must ensure that
            /// - the CPU ID is valid, and
            /// - data races will not happen.
            #[inline]
            pub unsafe fn remote_ptr(&self, cpu_id: usize) -> *const #ty {
                let base = percpu::percpu_area_base(cpu_id);
                let offset = #offset;
                (base + offset) as *const #ty
            }

            /// Returns the reference of the per-CPU static variable on the given CPU.
            ///
            /// # Safety
            ///
            /// Caller must ensure that
            /// - the CPU ID is valid, and
            /// - data races will not happen.
            #[inline]
            pub unsafe fn remote_ref_raw(&self, cpu_id: usize) -> &#ty {
                &*self.remote_ptr(cpu_id)
            }

            /// Returns the mutable reference of the per-CPU static variable on the given CPU.
            ///
            /// # Safety
            ///
            /// Caller must ensure that
            /// - the CPU ID is valid, and
            /// - data races will not happen.
            #[inline]
            #[allow(clippy::mut_from_ref)]
            pub unsafe fn remote_ref_mut_raw(&self, cpu_id: usize) -> &mut #ty {
                &mut *(self.remote_ptr(cpu_id) as *mut #ty)
            }

            #read_write_methods
        }
    }
    .into()
}

#[doc(hidden)]
#[cfg(not(feature = "sp-naive"))]
#[proc_macro]
pub fn percpu_symbol_offset(item: TokenStream) -> TokenStream {
    let symbol = &format_ident!("{}", item.to_string());
    let offset = arch::gen_offset(symbol);
    quote!({ #offset }).into()
}
