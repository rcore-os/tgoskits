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
#[cfg(target_arch = "loongarch64")]
pub mod loongarch64;
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
