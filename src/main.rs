#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate axlog;
extern crate alloc;
extern crate axruntime;

use alloc::{format, string::ToString};

mod entry;
mod mm;
mod syscall;

#[unsafe(no_mangle)]
fn main() {
    // Create a init process
    axprocess::Process::new_init(axtask::current().id().as_u64() as _).build();
    starry_core::vfs::mount_all().expect("Failed to mount vfs");

    let envs = [format!("ARCH={}", option_env!("ARCH").unwrap_or("unknown"))];

    let init = include_str!("init.sh");

    info!("Running init script");
    let args = ["/musl/busybox", "sh", "-c", init]
        .map(|s| s.to_string())
        .to_vec();
    let exit_code = entry::run_user_app(&args, &envs);
    info!("Init script exited with code: {:?}", exit_code);
}
