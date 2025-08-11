#![no_std]
#![feature(likely_unlikely)]
#![allow(missing_docs)]

#[macro_use]
extern crate axlog;

extern crate alloc;

pub mod file;
pub mod io;
pub mod mm;
pub mod signal;
pub mod socket;
pub mod syscall;
pub mod task;
pub mod terminal;
pub mod time;
pub mod vfs;

/// Initialize.
pub fn init() {
    vfs::mount_all().expect("Failed to mount vfs");

    axtask::register_timer_callback(|_| {
        time::inc_irq_cnt();
    });

    starry_core::time::spawn_alarm_task();
}
