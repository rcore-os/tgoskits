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

use axbacktrace::Backtrace;
use std::println;

#[inline(never)]
fn c() {
    let bt = Backtrace::capture();
    println!("{}", bt.kind("raw"));
    core::hint::black_box(());
}

#[inline(never)]
fn b() {
    let f: fn() = c;
    f();
    core::hint::black_box(());
}

#[inline(never)]
fn a() {
    let f: fn() = b;
    f();
    core::hint::black_box(());
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("emitting raw backtrace report (normal fp chain)...");
    a();
    println!("test pass");
    #[cfg(feature = "ax-std")]
    use std::os::arceos::modules::ax_hal;
    #[cfg(feature = "ax-std")]
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
