//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![feature(likely_unlikely)]
#![feature(bstr)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

extern crate alloc;

#[macro_use]
extern crate axlog;

pub mod config;
pub mod file;
pub mod mm;
pub mod pseudofs;
pub mod syscall;
pub mod task;
pub mod time;

/// Initialize.
pub fn init() {
    info!("Initialize pseudofs...");
    pseudofs::mount_all().expect("Failed to mount pseudofs");

    info!("Initialize alarm...");
    task::spawn_alarm_task();
}
