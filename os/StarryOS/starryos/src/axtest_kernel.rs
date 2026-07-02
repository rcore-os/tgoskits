#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(feature = "axtest-kernel")]
use core::fmt::Arguments;

use ax_std as _;

#[cfg(feature = "axtest-kernel")]
fn axtest_print(args: Arguments<'_>) {
    ax_std::print!("{}", args);
}

#[cfg(feature = "axtest-kernel")]
#[cfg_attr(target_os = "none", unsafe(no_mangle))]
fn main() {
    starry_kernel::axtest_support::link();
    axtest::set_printer(axtest_print);
    axtest::set_coverage_wait_fn(wait_for_coverage_extraction);
    let summary = axtest::init().run_tests();
    if summary.failed == 0 {
        axtest::dump_coverage();
        ax_std::println!("AXTEST_SUITE_OK");
        ax_hal::power::system_off();
    } else {
        panic!("AXTEST_SUITE_FAIL failed={}", summary.failed);
    }
}

#[cfg(feature = "axtest-kernel")]
fn wait_for_coverage_extraction() {
    const WAIT_NANOS: u64 = 5_000_000_000;
    let start = ax_hal::time::wall_time_nanos();
    while ax_hal::time::wall_time_nanos().saturating_sub(start) < WAIT_NANOS {
        core::hint::spin_loop();
    }
}

#[cfg(not(feature = "axtest-kernel"))]
fn main() {
    compile_error!("starryos-axtest-kernel requires the axtest-kernel feature");
}
