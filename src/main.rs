#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate axlog;

extern crate alloc;
extern crate axruntime;

use alloc::{borrow::ToOwned, format, vec::Vec};

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
    let envs = [
        format!("ARCH={}", option_env!("ARCH").unwrap_or("unknown")),
        "HOSTNAME=starry".to_owned(),
        "HOME=/root".to_owned(),
    ];
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

#[cfg(feature = "2k1000la")]
extern crate axplat_loongarch64_2k1000la;
