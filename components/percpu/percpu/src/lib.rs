#![cfg_attr(not(feature = "host-test"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

extern crate ax_percpu_macros;
extern crate self as ax_percpu;

mod alignment;
mod area;
#[cfg(feature = "host-test")]
pub mod host_test;
mod initialization;
mod template;
mod value;

pub(crate) use alignment::required_area_alignment;
pub use ax_percpu_macros::def_percpu;
pub use initialization::initialize_layout;
pub(crate) use template::{template_base, template_size};

pub use self::{
    area::*,
    value::{ObjectAccess, PerCpu, PrimitiveAccess},
};

#[doc(hidden)]
pub mod __priv {
    pub use crate::{
        initialization::{PerCpuInitDescriptor, PerCpuInitRegistration},
        value::PerCpuSymbol,
    };

    /// Calculates one symbol's offset from the per-CPU template header.
    #[inline(always)]
    pub fn symbol_offset(symbol_address: usize) -> usize {
        symbol_address
            .checked_sub(crate::template_base())
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
        let binding = cpu_local::platform::current_cpu_binding()
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
