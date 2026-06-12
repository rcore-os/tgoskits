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
#[cfg_attr(not(feature = "ebpf-kmod"), allow(dead_code))]
mod ebpf;
mod file;
#[cfg_attr(not(feature = "ebpf-kmod"), allow(dead_code))]
mod kmod;
#[cfg_attr(not(feature = "ebpf-kmod"), allow(dead_code))]
mod kprobe;
mod mm;
#[cfg_attr(not(feature = "ebpf-kmod"), allow(dead_code))]
mod perf;
mod pseudofs;
mod stop_machine;
mod syscall;
mod task;
mod time;
mod tracepoint;
mod trap;
#[cfg_attr(not(feature = "ebpf-kmod"), allow(dead_code))]
mod uprobe;
