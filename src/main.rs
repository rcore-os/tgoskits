#![no_std]
#![no_main]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate axlog;

extern crate alloc;
extern crate axruntime;

use alloc::{borrow::ToOwned, format, vec::Vec};

mod entry;
mod test;

#[unsafe(no_mangle)]
fn main() {
    starry_api::init();

    let args = test::CMDLINE
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
}

#[cfg(feature = "vf2")]
extern crate axplat_riscv64_visionfive2;

#[cfg(feature = "vf2")]
axdriver::include_initrd!("vf2.img");
