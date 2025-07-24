//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

#[macro_use]
extern crate axlog;

pub mod config;
pub mod futex;
pub mod mm;
pub mod resources;
pub mod shm;
pub mod task;
pub mod terminal;
pub mod time;
pub mod vfs;

/// Initialize.
pub fn init(
    devfs_extra: impl FnOnce(&alloc::sync::Arc<vfs::SimpleFs>, &mut vfs::DirMapping<axsync::RawMutex>),
) {
    vfs::mount_all(devfs_extra).expect("Failed to mount vfs");

    axtask::register_timer_callback(|_| {
        time::inc_irq_cnt();
        task::poll_timer(&axtask::current());
    });
}
