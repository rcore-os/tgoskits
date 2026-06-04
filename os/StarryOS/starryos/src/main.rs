#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![doc = include_str!("../../README.md")]

extern crate alloc;

use alloc::{borrow::ToOwned, vec::Vec};

#[cfg(arceos_std)]
use ax_std as _;

pub const CMDLINE: &[&str] = &["/bin/sh", "-c", include_str!("init.sh")];

#[cfg_attr(target_os = "none", unsafe(no_mangle))]
fn main() {
    let args = CMDLINE
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let envs = [];

    starry_kernel::entry::init(&args, &envs);
}
