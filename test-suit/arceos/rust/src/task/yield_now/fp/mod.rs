//! ABI-preserved hardware FP state across overlapping same-CPU tasks.
use std::{
    sync::{Arc, Barrier},
    thread,
    vec::Vec,
};

use ax_std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};

cfg_select! {
    target_arch = "x86_64" => {
        mod x86_64;
        use x86_64 as registers;
    }
    target_arch = "aarch64" => {
        mod aarch64;
        use aarch64 as registers;
    }
    target_arch = "riscv64" => {
        mod riscv64;
        use riscv64 as registers;
    }
    target_arch = "loongarch64" => {
        mod loongarch64;
        use loongarch64 as registers;
    }
}

pub(super) fn run() {
    let cpu_count = ax_std::os::arceos::task::sched::cpu_topology_len().unwrap();
    let cpu = usize::from(cpu_count > 1);
    let overlap = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for index in 0..2 {
        let overlap = Arc::clone(&overlap);
        workers.push(thread::spawn(move || {
            ax_set_current_affinity(AxCpuMask::one_shot(cpu)).unwrap();
            let previous = registers::read();
            let expected = registers::value(previous, index);
            registers::write(expected);
            // Both distinct hardware states must coexist before verification;
            // plain yields could otherwise finish one task without switching.
            overlap.wait();
            for _ in 0..16 {
                thread::yield_now();
                assert!(
                    registers::matches(registers::read(), expected),
                    "hardware FP state was not preserved"
                );
            }
            overlap.wait();
            registers::write(previous);
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
}
