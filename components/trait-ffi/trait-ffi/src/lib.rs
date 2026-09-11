//! Statically linked interfaces implemented in a separate crate.
//!
//! `def_extern_trait` generates a calling module and its `impl_trait!` macro.
//! Bind one provider in the final binary, or opt into `weak_default` for
//! provider-free defaults. Rust ABI interfaces require
//! one compatible Rust build; this is not a stable dynamic-plugin ABI.
//!
//! ```rust,standalone_crate
//! use trait_ffi::{call_interface, def_extern_trait, impl_extern_trait};
//!
//! #[def_extern_trait]
//! pub trait Clock {
//!     fn ticks() -> u64;
//! }
//!
//! struct Platform;
//! #[impl_extern_trait]
//! impl Clock for Platform {
//!     fn ticks() -> u64 {
//!         42
//!     }
//! }
//!
//! fn main() {
//!     assert_eq!(call_interface!(Clock::ticks()), 42);
//! }
//! ```
#![no_std]

pub use trait_ffi_macros::{call_interface, def_extern_trait, impl_extern_trait};
