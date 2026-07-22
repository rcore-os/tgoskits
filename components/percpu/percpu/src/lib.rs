#![cfg_attr(not(feature = "host-test"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

extern crate ax_percpu_macros;
extern crate self as ax_percpu;

mod alignment;
mod area;
mod descriptor;
mod error;
mod ffi;
#[cfg(feature = "host-test")]
pub mod host_test;
mod initialization;
mod layout;
mod region;
mod template;
mod value;

pub(crate) use alignment::required_area_alignment;
pub use ax_percpu_macros::def_percpu;
pub use cpu_local::{CpuAreaRef, CpuIndex, CpuPin, ExclusiveCpu, with_cpu_pin, with_exclusive_cpu};
pub(crate) use template::{template_base, template_size};

pub use self::{
    area::{PerCpuArea, area, current_area, current_cpu_index, layout},
    error::PerCpuError,
    initialization::initialize_layout,
    layout::PerCpuLayout,
    region::PerCpuRegion,
    value::PerCpu,
};

#[doc(hidden)]
pub mod __priv {
    pub use crate::{
        descriptor::{PerCpuInitDescriptor, PerCpuInitRegistration},
        value::{PerCpuObjectSymbol, PerCpuPrimitiveSymbol, PerCpuSymbol},
    };

    /// Calculates one symbol's offset from the template prefix.
    pub fn symbol_offset(symbol_address: usize) -> usize {
        symbol_address
            .checked_sub(crate::template_base())
            .expect("per-CPU symbol must follow the loaded template prefix")
    }

    /// Calculates a symbol pointer covered by an explicit CPU pin.
    pub fn current_symbol_ptr<T>(pin: &crate::CpuPin<'_>, offset: usize) -> core::ptr::NonNull<T> {
        // SAFETY: macro-generated offsets were validated before layout
        // publication and CpuPin carries an initialized permanent area.
        unsafe { core::ptr::NonNull::new_unchecked((pin.area().base() + offset) as *mut T) }
    }

    /// Calculates a symbol pointer in an explicit remote area.
    pub fn remote_symbol_ptr<T>(area: crate::PerCpuArea, offset: usize) -> core::ptr::NonNull<T> {
        crate::area::symbol_ptr(area, offset)
    }
}

/// Example per-CPU data for documentation only.
#[cfg(doc)]
#[cfg_attr(docsrs, doc(cfg(doc)))]
#[def_percpu]
pub static EXAMPLE_PERCPU_DATA: usize = 0;
