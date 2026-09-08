//! Statically linked interfaces implemented in a separate crate.
//!
//! `def_extern_trait` generates a calling module and its `impl_trait!` macro.
//! Bind exactly one provider in the final binary. Rust ABI interfaces require
//! one compatible Rust build; this is not a stable dynamic-plugin ABI.
#![no_std]

pub use trait_ffi_macros::def_extern_trait;
