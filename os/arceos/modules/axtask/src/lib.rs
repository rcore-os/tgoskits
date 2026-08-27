//! [ArceOS](https://github.com/arceos-org/arceos) task management module.
//!
//! This module provides primitives for task management, including task
//! creation, scheduling, sleeping, termination, etc. The scheduler algorithm
//! is configurable by cargo features.
//!
//! # Cargo Features
//!
//! Multi-task scheduling and interrupt handling are mandatory runtime
//! capabilities. Timer-based APIs such as [`sleep`], [`sleep_until`], and
//! [`WaitQueue::wait_timeout`] are always available.
//! - `preempt`: Enable preemptive scheduling.
//! - FIFO cooperative scheduler is the default when no scheduler feature is
//!   selected.
//! - `sched-rr`: Use the [Round-robin preemptive scheduler][2]. It also enables
//!   the `preempt` feature.
//! - `sched-cfs`: Use the [Completely Fair Scheduler][3]. It also enables the
//!   `preempt` feature.
//! - `host-test`: Use host-safe fallbacks for unit tests.
//!
//! [1]: ax_sched::FifoScheduler
//! [2]: ax_sched::RRScheduler
//! [3]: ax_sched::CFScheduler

#![cfg_attr(any(not(test), target_os = "none"), no_std)]
#![cfg_attr(all(test, target_os = "none"), no_main)]
#![cfg_attr(all(test, target_os = "none"), feature(custom_test_frameworks))]
#![cfg_attr(doc, feature(doc_cfg))]
#![cfg_attr(
    all(test, target_os = "none"),
    test_runner(crate::bare_metal_test_runner)
)]

#[cfg(all(feature = "host-test", not(target_os = "none")))]
extern crate std;

/// Native ArceOS synchronization primitives.
pub mod sync;

#[cfg(all(test, target_os = "none"))]
fn bare_metal_test_runner(_tests: &[&dyn Fn()]) {}

#[cfg(all(test, target_os = "none"))]
#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(all(test, target_os = "none"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

mod build_info {
    include!(concat!(env!("OUT_DIR"), "/build_info.rs"));
}

#[macro_use]
extern crate log;
extern crate alloc;

#[macro_use]
mod run_queue;
mod api;
mod interrupt;
mod irq_notify;
#[cfg(feature = "lockdep")]
mod lockdep;
#[doc(hidden)]
pub mod runtime_preempt;
#[cfg(feature = "tracepoint-hooks")]
mod sched_tracepoint;
mod task;
mod timers;
mod wait_queue;

pub mod future;

#[cfg(all(feature = "smp", feature = "ipi"))]
pub use self::run_queue::handle_ipi_reschedule;
#[cfg(feature = "tracepoint-hooks")]
pub use self::sched_tracepoint::SchedTracepoint;
pub use self::{
    api::{sleep, sleep_until, yield_now, *},
    irq_notify::IrqNotify,
};
