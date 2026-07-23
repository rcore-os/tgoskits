#![cfg_attr(not(test), no_std)]
#![cfg_attr(doc, feature(doc_cfg))]

//! Shared page-table primitives with independently selectable consumers.

pub mod common;
pub mod entry;

#[cfg(feature = "stage1")]
#[cfg_attr(doc, doc(cfg(feature = "stage1")))]
pub mod stage1;

#[cfg(any(feature = "stage2", feature = "boot"))]
mod flexible;

/// Variable-level page tables used for guest stage-2 translation.
#[cfg(feature = "stage2")]
#[cfg_attr(doc, doc(cfg(feature = "stage2")))]
pub mod stage2 {
    pub use crate::flexible::*;
}

/// Allocation-provider page tables used before the runtime allocator exists.
#[cfg(feature = "boot")]
#[cfg_attr(doc, doc(cfg(feature = "boot")))]
pub mod boot {
    pub use crate::flexible::*;
}
