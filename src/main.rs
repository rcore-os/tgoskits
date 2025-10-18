#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate axlog;

extern crate alloc;
extern crate axruntime;

use alloc::{borrow::ToOwned, vec::Vec};

use axfs_ng::FS_CONTEXT;

mod entry;

pub const CMDLINE: &[&str] = &["/bin/sh", "-c", include_str!("init.sh")];

#[unsafe(no_mangle)]
fn main() {
    starry_api::init();

    let args = CMDLINE
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let envs = [];
    let exit_code = entry::run_initproc(&args, &envs);
    info!("Init process exited with code: {:?}", exit_code);

    let cx = FS_CONTEXT.lock();
    cx.root_dir()
        .unmount_all()
        .expect("Failed to unmount all filesystems");
    cx.root_dir()
        .filesystem()
        .flush()
        .expect("Failed to flush rootfs");
}

#[cfg(feature = "vf2")]
extern crate axplat_riscv64_visionfive2;
