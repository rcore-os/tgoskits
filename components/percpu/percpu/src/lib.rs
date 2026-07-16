#![cfg_attr(not(feature = "host-test"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

extern crate ax_percpu_macros;
extern crate self as ax_percpu;

#[cfg(not(feature = "sp-naive"))]
mod alignment;
mod area;
#[cfg(not(feature = "sp-naive"))]
mod initialization;
mod value;

#[cfg(all(not(feature = "sp-naive"), not(feature = "custom-base")))]
mod linked_layout;
#[cfg(feature = "sp-naive")]
#[path = "naive.rs"]
mod storage;
#[cfg(all(not(feature = "sp-naive"), feature = "custom-base"))]
#[path = "custom/mod.rs"]
mod storage;

#[cfg(not(feature = "sp-naive"))]
pub(crate) use alignment::required_area_alignment;
pub use ax_percpu_macros::def_percpu;
#[cfg(not(feature = "sp-naive"))]
pub use initialization::initialize_layout;

#[cfg(all(not(feature = "sp-naive"), not(feature = "custom-base")))]
pub use self::linked_layout::{init, linker_layout};
#[cfg(all(not(feature = "sp-naive"), not(feature = "custom-base")))]
pub(crate) use self::linked_layout::{percpu_area_size, percpu_template_base};
#[cfg(any(feature = "sp-naive", feature = "custom-base"))]
pub use self::storage::init;
#[cfg(any(feature = "sp-naive", feature = "custom-base"))]
pub(crate) use self::storage::{percpu_area_size, percpu_template_base};
pub use self::{
    area::*,
    value::{ObjectAccess, PerCpu, PrimitiveAccess},
};

#[cfg(feature = "sp-naive")]
fn required_area_alignment() -> Result<usize, PerCpuError> {
    Ok(core::mem::align_of::<CpuAreaPrefix>())
}

#[doc(hidden)]
pub mod __priv {
    #[cfg(not(feature = "sp-naive"))]
    pub use crate::initialization::{PerCpuInitDescriptor, PerCpuInitRegistration};
    pub use crate::value::PerCpuSymbol;

    /// Calculates one symbol's offset from the per-CPU template header.
    #[inline(always)]
    pub fn symbol_offset(symbol_address: usize) -> usize {
        symbol_address
            .checked_sub(crate::percpu_template_base())
            .expect("per-CPU symbol must follow the loaded template prefix")
    }

    /// Calculates a symbol address covered by an explicit CPU pin.
    #[inline(always)]
    pub fn current_symbol_ptr<T>(pin: &crate::BoundCpuPin<'_>, offset: usize) -> *const T {
        pin.area_base().wrapping_add(offset) as *const T
    }

    /// Calculates the current runtime address of one per-CPU symbol.
    ///
    /// # Safety
    ///
    /// The caller must keep the current execution context pinned until the
    /// returned pointer is no longer used.
    #[inline(always)]
    pub unsafe fn current_symbol_ptr_unchecked<T>(offset: usize) -> *const T {
        let binding = ax_cpu_local::platform::current_cpu_binding()
            .expect("unchecked CPU-local access requires a platform-published binding");
        let area_base = binding.area_base;
        area_base.wrapping_add(offset) as *const T
    }
}

/// Example per-CPU data for documentation only.
#[cfg(doc)]
#[cfg_attr(docsrs, doc(cfg(doc)))]
#[def_percpu]
pub static EXAMPLE_PERCPU_DATA: usize = 0;
