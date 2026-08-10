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
    struct CriticalSectionOpsImpl;

    #[ax_crate_interface::impl_interface]
    impl ax_sync::CriticalSectionOps for CriticalSectionOpsImpl {
        fn enable_preempt() {}

        fn disable_preempt() {}

        fn irq_save_and_disable() -> usize {
            1
        }

        fn irq_restore(_state: usize) {}
    }
}
