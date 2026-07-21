//! Typed ownership boundary for the host CPU-local architecture register.
//!
//! The crate owns only the register encoding and the fixed header visible to
//! early trap entry. Allocation, per-CPU area layout, scheduling, IRQ policy,
//! and task-local TLS remain in higher layers.

#![cfg_attr(not(any(test, feature = "host-test")), no_std)]

#[cfg(feature = "host-test")]
extern crate std;

mod header;
mod identity;
pub mod platform;
mod register;
mod symbol;

pub mod abi;

pub use abi::*;
pub use header::*;
pub use identity::*;
pub use register::{
    CpuLocalError, PreparedCurrentThreadPublish, commit_current_thread_publish,
    prepare_current_thread_publish, prepare_current_thread_publish_for_binding, runtime_anchor,
};
#[doc(hidden)]
pub use symbol::{cpu_area_template_base, cpu_area_template_size};

/// Architecture register allowlist used only by the platform binder/provider.
#[doc(hidden)]
pub mod raw {
    pub use crate::register::{
        current_area_base_raw, current_area_base_unchecked, current_cpu_binding as current_binding,
        current_thread, get_task_pointer_raw as get_task_pointer, install_binding,
        set_task_pointer_raw as set_task_pointer,
    };
}

/// LoongArch host scratch-register assignments shared by trap and vCPU code.
#[cfg(target_arch = "loongarch64")]
pub mod loongarch64 {
    /// Kernel stack pointer scratch slot used by exception entry.
    pub const KSAVE_KSP: usize = 0;
    /// First temporary-register scratch slot used by exception entry.
    pub const KSAVE_T0: usize = 1;
    /// Second temporary-register scratch slot used by exception entry.
    pub const KSAVE_T1: usize = 2;
    /// CPU-local runtime area-base shadow restored by exception entry.
    pub const KSAVE_PERCPU: usize = 3;

    /// Host CPU-local runtime area-base shadow.
    pub const HOST_PERCPU_KS: usize = KSAVE_PERCPU;
    /// Host stack scratch reserved for vCPU entry and exit.
    pub const HOST_VCPU_KS: usize = 4;
    /// Temporary scratch reserved for vCPU entry and exit.
    pub const HOST_VCPU_TMP_KS: usize = 5;
}
