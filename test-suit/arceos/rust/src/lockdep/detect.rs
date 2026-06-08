use std::println;

use ax_kspin::SpinRaw;

pub fn run() -> crate::TestResult {
    println!("lockdep_detect: triggering spin lock order inversion");
    spin_single_task_abba();
    panic!("lockdep did not report an expected lock order inversion");
}

fn spin_single_task_abba() {
    let lock_a = SpinRaw::new(0usize);
    let lock_b = SpinRaw::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("lockdep_detect: recorded spin A -> B");
    }

    let _guard_b = lock_b.lock();
    let _guard_a = lock_a.lock();
}
