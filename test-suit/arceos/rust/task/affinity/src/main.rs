#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};
#[cfg(feature = "ax-std")]
use std::os::arceos::modules::ax_hal::percpu::this_cpu_id;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

const NUM_TASKS: usize = 10;
const NUM_TIMES: usize = 100;
static FINISHED_TASKS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "ax-std")]
fn online_cpu_mask() -> AxCpuMask {
    let cpu_num = thread::available_parallelism().unwrap().get();
    let mut cpumask = AxCpuMask::new();
    for cpu_id in 0..cpu_num {
        cpumask.set(cpu_id, true);
    }
    cpumask
}

#[allow(clippy::modulo_one)]
#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    println!("Hello, main task!");
    let available_cpus = thread::available_parallelism().unwrap().get();
    for i in 0..NUM_TASKS {
        #[cfg(feature = "ax-std")]
        let cpu_id = i % available_cpus;
        #[cfg(not(feature = "ax-std"))]
        let _cpu_id = i % available_cpus;
        thread::spawn(move || {
            // Initialize cpu affinity here.
            #[cfg(feature = "ax-std")]
            assert!(
                ax_set_current_affinity(AxCpuMask::one_shot(cpu_id)).is_ok(),
                "Initialize CPU affinity failed!"
            );

            println!("Hello, task ({})! id = {:?}", i, thread::current().id());
            for _t in 0..NUM_TIMES {
                // Test CPU affinity here.
                #[cfg(feature = "ax-std")]
                assert_eq!(this_cpu_id(), cpu_id, "CPU affinity tests failed!");
                thread::yield_now();
            }

            // Change cpu affinity here.
            #[cfg(feature = "ax-std")]
            if available_cpus > 1 {
                let mut cpumask = online_cpu_mask();
                cpumask.set(cpu_id, false);
                assert!(
                    ax_set_current_affinity(cpumask).is_ok(),
                    "Change CPU affinity failed!"
                );

                for _t in 0..NUM_TIMES {
                    // Test CPU affinity here.
                    assert_ne!(this_cpu_id(), cpu_id, "CPU affinity changes failed!");
                    thread::yield_now();
                }
            }
            let _ = FINISHED_TASKS.fetch_add(1, Ordering::Relaxed);
        });
    }

    while FINISHED_TASKS.load(Ordering::Relaxed) < NUM_TASKS {
        thread::yield_now();
    }
    println!("All tests passed!");
}
