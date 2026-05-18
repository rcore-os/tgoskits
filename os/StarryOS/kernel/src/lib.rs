//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![feature(likely_unlikely)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

extern crate alloc;
extern crate ax_runtime;

#[macro_use]
extern crate ax_log;

#[macro_use]
pub mod dyn_debug; // Re-export debug macros for use in other modules. It will override the `debug` macro from `log` crate when `dynamic_debug` feature is enabled.

pub mod entry;

mod config;
mod file;
#[cfg(feature = "kcov")]
mod kcov;
mod mm;
mod pseudofs;
mod stop_machine;
mod syscall;
mod task;
mod time;
mod trap;
