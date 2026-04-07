#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

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

            let _order = FINISHED_TASKS.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "ax-std")]
            if cfg!(not(feature = "sched-cfs"))
                && thread::available_parallelism().unwrap().get() == 1
            {
                assert!(_order == i); // FIFO scheduler
            }
        });
    }
    println!("Hello, main task!");
    while FINISHED_TASKS.load(Ordering::Relaxed) < NUM_TASKS {
        #[cfg(all(not(feature = "sched-rr"), not(feature = "sched-cfs")))]
        thread::yield_now();
    }
    println!("All tests passed!");
}
