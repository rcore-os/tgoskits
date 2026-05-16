#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

#[cfg(any(not(target_os = "none"), feature = "ax-std"))]
macro_rules! app {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(any(not(target_os = "none"), feature = "ax-std")))]
macro_rules! app {
    ($($item:item)*) => {};
}

app! {

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

use std::println;

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    use std::os::arceos::modules::ax_hal;

    println!("Running backtrace tests...");
    let bt = axbacktrace::Backtrace::capture();
    println!("{}", bt.report("test"));
    println!("test pass");
    ax_hal::power::system_off();
}

}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
