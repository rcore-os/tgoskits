#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate axlog;
extern crate alloc;
extern crate axruntime;

use alloc::{borrow::ToOwned, format, vec::Vec};

use axerrno::LinuxError;
use starry_core::mm::insert_elf_cache;

mod entry;
mod mm;
mod syscall;

#[cfg(target_arch = "riscv64")]
const CACHED_ELFS: &[&str] = &[
    "/musl/busybox",
    "/glibc/busybox",
    "/musl/lib/libc.so",
    "/glibc/lib/ld-linux-riscv64-lp64d.so.1",
];

#[cfg(target_arch = "loongarch64")]
const CACHED_ELFS: &[&str] = &[
    "/musl/busybox",
    "/glibc/busybox",
    "/musl/lib/libc.so",
    "/glibc/lib/ld-linux-loongarch-lp64d.so.1",
];

#[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
const CACHED_ELFS: &[&str] = &[];

const ENTRY: &[&str] = &["/musl/busybox", "sh", "-c", include_str!("init.sh")];
// const ENTRY: &[&str] = &["/bin/sh"];

#[unsafe(no_mangle)]
fn main() {
    starry_core::vfs::mount_all().expect("Failed to mount vfs");

    for elf in CACHED_ELFS {
        match insert_elf_cache(elf) {
            Ok(_) | Err(LinuxError::ENOENT) => {}
            Err(err) => error!("Failed to insert ELF cache for {}: {}", elf, err),
        }
    }

    let args = ENTRY.iter().copied().map(str::to_owned).collect::<Vec<_>>();
    let envs = [format!("ARCH={}", option_env!("ARCH").unwrap_or("unknown"))];
    let exit_code = entry::run_initproc(&args, &envs);
    info!("Init process exited with code: {:?}", exit_code);
}
