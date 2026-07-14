//! Typed ownership boundary for the host CPU-local architecture register.
//!
//! The crate owns only the register encoding and the fixed header visible to
//! early trap entry. Allocation, per-CPU area layout, scheduling, IRQ policy,
//! and task-local TLS remain in higher layers.

#![cfg_attr(not(any(test, feature = "host-test")), no_std)]

#[cfg(feature = "host-test")]
extern crate std;

mod header;
mod register;
mod relocation;
mod symbol;

pub use header::*;
pub use register::*;
pub use relocation::*;
#[doc(hidden)]
pub use symbol::{cpu_area_header_link_address, cpu_area_template_size};

/// LoongArch host scratch-register assignments shared by trap and vCPU code.
#[cfg(target_arch = "loongarch64")]
pub mod loongarch64 {
    /// Kernel stack pointer scratch slot used by exception entry.
    pub const KSAVE_KSP: usize = 0;
    /// First temporary-register scratch slot used by exception entry.
    pub const KSAVE_T0: usize = 1;
    /// Second temporary-register scratch slot used by exception entry.
    pub const KSAVE_T1: usize = 2;
    /// CPU-local relocation shadow restored by exception entry.
    pub const KSAVE_PERCPU: usize = 3;

    /// Host CPU-local relocation shadow.
    pub const HOST_PERCPU_KS: usize = KSAVE_PERCPU;
    /// Host stack scratch reserved for vCPU entry and exit.
    pub const HOST_VCPU_KS: usize = 4;
    /// Temporary scratch reserved for vCPU entry and exit.
    pub const HOST_VCPU_TMP_KS: usize = 5;
}
