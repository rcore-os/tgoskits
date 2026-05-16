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

fn panic_path() {
    println!("triggering panic to exercise panic backtrace path...");
    let f: fn() = a;
    f();
    panic!("backtrace panic-path smoke test");
}

#[inline(never)]
fn c() {
    let bt = Backtrace::capture();
    println!("{}", bt.report("raw"));
}

#[inline(never)]
fn b() {
    let f: fn() = c;
    f();
}

#[inline(never)]
fn a() {
    let f: fn() = b;
    f();
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    if cfg!(feature = "panic-path") {
        panic_path();
    } else {
        println!("emitting raw backtrace report...");
        a();
        println!("test pass");
        std::os::arceos::modules::ax_hal::power::system_off();
    }
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
