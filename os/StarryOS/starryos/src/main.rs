#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![doc = include_str!("../../README.md")]

extern crate alloc;

#[cfg(not(axtest))]
use alloc::{borrow::ToOwned, vec::Vec};

use ax_std as _;

pub const CMDLINE: &[&str] = &["/bin/sh", "-c", include_str!("init.sh")];

#[cfg_attr(target_os = "none", unsafe(no_mangle))]
#[cfg(not(axtest))]
fn main() {
    let args = CMDLINE
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let envs = [];

    starry_kernel::entry::init(&args, &envs);
}

#[cfg(axtest)]
#[cfg_attr(target_os = "none", unsafe(no_mangle))]
fn main() {
    use core::fmt::Arguments;

    fn print(args: Arguments<'_>) {
        ax_std::print!("{}", args);
    }

    starry_kernel::init_axtest_linkage();
    axtest::set_printer(print);
    let summary = axtest::init().run_tests();
    if summary.failed == 0 {
        axtest::dump_coverage();
        ax_std::println!("AXTEST_SUITE_OK");
        ax_hal::power::system_off();
    } else {
        panic!("AXTEST_SUITE_FAIL failed={}", summary.failed);
    }
}
