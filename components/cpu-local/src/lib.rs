#![doc = include_str!("../README.md")]
#![cfg_attr(not(any(test, feature = "host-test")), no_std)]

#[cfg(feature = "host-test")]
extern crate std;

mod area;
mod error;
mod identity;
#[cfg(target_arch = "loongarch64")]
pub mod loongarch64;
mod pin;
mod register;
mod switch;
mod symbol;
mod thread;

pub use area::*;
pub use error::*;
pub use identity::*;
pub use pin::*;
pub use register::current_thread;
#[cfg(feature = "tls")]
#[doc(hidden)]
pub use register::install_kernel_tls;
#[cfg(feature = "tls")]
pub use register::kernel_tls;
#[doc(hidden)]
pub use register::{
    install_bootstrap_thread, install_cpu_area, scheduler_clear_preempt_need_resched,
    scheduler_consume_final_preempt_guard, scheduler_current_cpu_index, scheduler_current_thread,
    scheduler_enter_preempt_guard, scheduler_preempt_guard_depth,
    scheduler_prepare_preempt_guard_exit, scheduler_set_preempt_need_resched,
};
pub use switch::{PreparedThreadSwitch, PreviousThreadBinding, prepare_thread_switch};
#[doc(hidden)]
pub use symbol::{cpu_area_template_base, cpu_area_template_size};
pub use thread::*;

/// Host-only observations of the modeled architecture register boundary.
#[cfg(feature = "host-test")]
#[doc(hidden)]
pub mod host_test {
    pub use crate::register::host_test::{
        RegisterReadCounts, register_read_counts, reset_register_read_counts,
    };
}
