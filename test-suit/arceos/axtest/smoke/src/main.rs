#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use core::fmt::Arguments;

#[cfg(all(feature = "ax-std", axtest))]
use axtest::prelude::*;

#[cfg(feature = "ax-std")]
fn axtest_print(args: Arguments<'_>) {
    ax_std::print!("{}", args);
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
#[cfg(feature = "ax-std")]
fn main() {
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

fn wait_for_coverage_extraction() {
    // Give the host enough time to read the profraw via the QEMU monitor
    // before we proceed to system_off. CI runs QEMU without KVM, where a
    // ~30 MB memsave takes well under a second; 5 s is a comfortable cap.
    use ax_std::time::{Duration, Instant};
    const WAIT: Duration = Duration::from_secs(5);
    let start = Instant::now();
    while start.elapsed() < WAIT {
        core::hint::spin_loop();
    }
}

#[cfg(axtest)]
mod smoke {
    use super::*;

    #[axtest]
    fn arithmetic_smoke() {
        ax_assert_eq!(2 + 2, 4);
    }

    #[axtest]
    fn explicit_result_smoke() -> axtest::AxTestResult {
        ax_assert!(true);
        axtest::AxTestResult::Ok
    }
}

#[cfg(not(feature = "ax-std"))]
fn main() {
    eprintln!("arceos-axtest-suit requires the ax-std feature for kernel runs");
}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
