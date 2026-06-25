use std::println;

use ax_kspin::SpinRaw;

pub fn run() -> crate::TestResult {
    println!("lockdep_spin_detect: triggering spin::Mutex and ax_kspin order inversion");
    mixed_spin_mutex_single_task_abba();
    panic!("lockdep did not report an expected spin::Mutex lock order inversion");
}

fn mixed_spin_mutex_single_task_abba() {
    let lock_a = spin::Mutex::new(0usize);
    let lock_b = SpinRaw::new(0usize);

    {
        let _guard_a = lock_a.lock();
        let _guard_b = lock_b.lock();
        println!("lockdep_spin_detect: recorded spin::Mutex A -> ax_kspin B");
    }

    let _guard_b = lock_b.lock();
    let _guard_a = lock_a.lock();
}
