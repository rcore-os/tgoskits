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

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use std::{
    sync::{Arc, Mutex},
    thread,
};

#[cfg(feature = "ax-std")]
fn trigger_lock_order_inversion() {
    let lock_a = Arc::new(Mutex::new(0usize));
    let lock_b = Arc::new(Mutex::new(0usize));

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("Recorded lock order: A -> B");
    }

    let held_a = lock_a.lock();
    let thread_lock_a = lock_a.clone();
    let thread_lock_b = lock_b.clone();

    let handle = thread::spawn(move || {
        let _guard_b = thread_lock_b.lock();
        let guard_a = thread_lock_a.try_lock();
        assert!(
            guard_a.is_none(),
            "try_lock(A) unexpectedly succeeded while A was still held",
        );
        println!("Lock inversion went unnoticed without lockdep, as expected");
    });

    handle.join().unwrap();
    drop(held_a);
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("lockdep regression test start");

    #[cfg(feature = "ax-std")]
    trigger_lock_order_inversion();

    #[cfg(feature = "lockdep")]
    panic!(
        "lockdep feature was enabled for the test app, but no lock order inversion was reported"
    );

    #[cfg(not(feature = "lockdep"))]
    println!("All tests passed!");
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
