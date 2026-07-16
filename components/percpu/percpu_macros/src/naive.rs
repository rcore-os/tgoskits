//! For single CPU use, we just make the per-CPU data a global variable.

use quote::quote;
use syn::{Ident, Type};

pub fn gen_offset(_symbol: &Ident) -> proc_macro2::TokenStream {
    quote!(0)
}

pub fn gen_current_ptr(symbol: &Ident, _ty: &Type) -> proc_macro2::TokenStream {
    quote! {
        ::core::ptr::addr_of!(#symbol) as *const _
    }
}

pub fn gen_current_ptr_pinned(symbol: &Ident, pin: &Ident, _ty: &Type) -> proc_macro2::TokenStream {
    quote! {{
        let _ = #pin;
        ::core::ptr::addr_of!(#symbol) as *const _
    }}
}

pub fn gen_remote_ptr(symbol: &Ident, cpu_index: &Ident, _ty: &Type) -> proc_macro2::TokenStream {
    quote! {
        if #cpu_index.as_u32() == 0 {
            Ok(::core::ptr::addr_of!(#symbol) as *const _)
        } else {
            Err(ax_percpu::PerCpuError::CpuOutOfRange {
                cpu_index: #cpu_index,
                area_count: 1,
            })
        }
    }
}
