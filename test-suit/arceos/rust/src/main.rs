#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use std::{println, time::Instant};

#[cfg(feature = "ax-std")]
use arceos_test_suit::selected_tests;

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
#[cfg(feature = "ax-std")]
fn main() {
    let tests = selected_tests();
    assert!(!tests.is_empty(), "no ArceOS test suite feature selected");

    println!("ArceOS test suite run begin: {} tests", tests.len());
    for test in tests {
        let started = Instant::now();
        println!(
            "ARCEOS_TEST_BEGIN feature={} name={}",
            test.feature, test.name
        );
        match (test.run)() {
            Ok(()) => {
                println!(
                    "ARCEOS_TEST_END feature={} name={} status=pass elapsed_ms={}",
                    test.feature,
                    test.name,
                    started.elapsed().as_millis()
                );
            }
            Err(message) => {
                println!(
                    "ARCEOS_TEST_END feature={} name={} status=fail elapsed_ms={} reason={}",
                    test.feature,
                    test.name,
                    started.elapsed().as_millis(),
                    message
                );
                panic!(
                    "ARCEOS_TEST_FAIL feature={} reason={}",
                    test.feature, message
                );
            }
        }
    }
    println!("ArceOS test suite run OK!");
    ax_hal::power::system_off();
}

#[cfg(not(feature = "ax-std"))]
fn main() {
    eprintln!("arceos-test-suit requires an ArceOS feature such as `all` for kernel runs");
}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
