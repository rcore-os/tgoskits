#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(all(feature = "ax-std", axtest))]
#[axtest::tests]
mod smoke {
    use axtest::prelude::*;

    #[test]
    fn arithmetic_smoke() {
        ax_assert_eq!(2 + 2, 4);
    }

    #[test]
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
