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

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

const NUM_TASKS: usize = 10;
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    for i in 0..NUM_TASKS {
        thread::spawn(move || {
            println!("Hello, task {}! id = {:?}", i, thread::current().id());

            #[cfg(all(not(feature = "sched-rr"), not(feature = "sched-cfs")))]
            thread::yield_now();

            let _order = FINISHED_TASKS.fetch_add(1, Ordering::Release);
            #[cfg(feature = "ax-std")]
            if cfg!(not(feature = "sched-cfs"))
                && thread::available_parallelism().unwrap().get() == 1
            {
                assert!(_order == i); // FIFO scheduler
            }
        });
    }
    println!("Hello, main task!");
    while FINISHED_TASKS.load(Ordering::Acquire) < NUM_TASKS {
        #[cfg(all(not(feature = "sched-rr"), not(feature = "sched-cfs")))]
        thread::yield_now();
    }
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
