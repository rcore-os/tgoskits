#![doc = include_str!("../README.md")]
#![cfg_attr(not(any(test, feature = "host-test")), no_std)]

#[cfg(feature = "host-test")]
extern crate std;

mod area;
#[cfg(any(target_arch = "x86_64", target_arch = "riscv64"))]
mod cpu_entry;
mod error;
mod identity;
mod pin;
mod preempt;
mod register;
mod switch;
mod symbol;
mod thread;

pub use area::*;
pub use error::*;
pub use identity::*;
pub use pin::*;
pub use preempt::*;
pub use register::current_context;
#[doc(hidden)]
pub use register::current_cpu_index;
#[cfg(kernel_tls)]
#[doc(hidden)]
pub use register::install_kernel_tls;
#[cfg(kernel_tls)]
pub use register::kernel_tls;
#[doc(hidden)]
pub use register::{
    current_context_unpinned, install_bootstrap_context, install_cpu_area,
    is_permanent_boot_context,
};
pub use switch::{PreparedContextSwitch, PreviousContextBinding, prepare_context_switch};
#[doc(hidden)]
pub use symbol::{cpu_area_template_base, cpu_area_template_size};
pub use thread::*;

/// Host-only observations of the modeled architecture-register boundary.
#[cfg(feature = "host-test")]
#[doc(hidden)]
pub mod host_test {
    pub use crate::register::host_test::{
        RegisterReadCounts, register_read_counts, reset_register_read_counts,
    };
}
