#![no_std]
#![no_main]
#![doc = include_str!("../../README.md")]

extern crate alloc;

use alloc::{borrow::ToOwned, vec::Vec};

pub const CMDLINE: &[&str] = &["/bin/sh", "-c", include_str!("init.sh")];

#[unsafe(no_mangle)]
fn main() {
    let args = CMDLINE
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let envs = [];

    starry_kernel::entry::init(&args, &envs);
}

#[cfg(all(
    feature = "sg2002",
    any(target_arch = "riscv32", target_arch = "riscv64")
))]
extern crate ax_plat_riscv64_sg2002;

#[cfg(all(feature = "vf2", target_arch = "riscv64"))]
extern crate axplat_riscv64_visionfive2;
