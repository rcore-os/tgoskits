#![doc = include_str!("../README.md")]
#![no_std]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;

#[cfg(all(axtest, feature = "axtest"))]
/// Coverage tests for scoped local storage.
pub mod axtest;

mod boxed;
mod item;
mod scope;

pub use item::{Item, LocalItem, ScopeItem, ScopeItemMut};
pub use scope::{ActiveScope, Scope};

#[cfg(test)]
mod tests {
    struct KernelGuardIfImpl;

    #[ax_crate_interface::impl_interface]
    impl ax_kernel_guard::KernelGuardIf for KernelGuardIfImpl {
        fn enable_preempt() {}

        fn disable_preempt() {}
    }
}
