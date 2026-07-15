use quote::quote;
use syn::{Ident, Type};

/// Generates the byte offset of one symbol from the per-CPU template header.
pub fn gen_offset(symbol: &Ident) -> proc_macro2::TokenStream {
    quote! {
        ax_percpu::__priv::symbol_offset(::core::ptr::addr_of!(#symbol).cast::<u8>() as usize)
    }
}

/// Generates a pointer using the current CPU-area base and template offset.
pub fn gen_current_ptr(symbol: &Ident, ty: &Type) -> proc_macro2::TokenStream {
    let offset = gen_offset(symbol);
    quote! {
        ax_percpu::__priv::current_symbol_ptr_unchecked::<#ty>(#offset)
    }
}

/// Generates a pointer through an explicit current-CPU pin.
pub fn gen_current_ptr_pinned(symbol: &Ident, pin: &Ident, ty: &Type) -> proc_macro2::TokenStream {
    let offset = gen_offset(symbol);
    quote! {
        ax_percpu::__priv::current_symbol_ptr::<#ty>(#pin, #offset)
    }
}

/// Generates a typed remote pointer through the immutable installed layout.
pub fn gen_remote_ptr(symbol: &Ident, cpu_index: &Ident, ty: &Type) -> proc_macro2::TokenStream {
    let offset = gen_offset(symbol);
    quote! {
        {
            let base = ax_percpu::area(#cpu_index)?.runtime_base();
            Ok((base + #offset) as *const #ty)
        }
    }
}
