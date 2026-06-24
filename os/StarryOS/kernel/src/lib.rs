//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![feature(likely_unlikely)]
#![feature(c_variadic)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

extern crate alloc;
extern crate ax_runtime;

#[macro_use]
extern crate ax_log;

#[macro_use]
pub mod dyn_debug; // Re-export debug macros for use in other modules. It will override the `debug` macro from `log` crate when `dynamic_debug` feature is enabled.

pub mod entry;

mod cgroup;
mod config;
mod ebpf;
mod file;
mod kmod;
pub mod kprobe;
mod mm;
mod perf;
mod pseudofs;
mod stop_machine;
mod syscall;
mod task;
mod time;
mod tracepoint;
mod trap;
mod uprobe;

#[cfg(axtest)]
pub fn init_axtest_linkage() {}

#[cfg(axtest)]
mod axtests {
    use axtest::prelude::*;

    #[axtest]
    fn arithmetic_smoke() {
        ax_assert_eq!(2 + 2, 4);
    }

    #[axtest]
    fn kernel_result_smoke() -> axtest::AxTestResult {
        ax_assert!(true);
        axtest::AxTestResult::Ok
    }
}
