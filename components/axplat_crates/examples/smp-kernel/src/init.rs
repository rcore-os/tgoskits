use core::sync::atomic::{AtomicUsize, Ordering::Acquire};

use crate::CPU_NUM;

/// Number of CPUs finished initialization.
pub static INITED_CPUS: AtomicUsize = AtomicUsize::new(0);
pub fn init_smp_ok() -> bool {
    INITED_CPUS.load(Acquire) == CPU_NUM
}

pub fn init_kernel(cpu_id: usize, arg: usize) {
    axplat::percpu::init_primary(cpu_id);

    // Initialize trap, console, time.
    axplat::init::init_early(cpu_id, arg);

    // Initialize platform peripherals, such as IRQ handlers.
    axplat::init::init_later(cpu_id, arg);
}

pub fn init_kernel_secondary(cpu_id: usize) {
    axplat::percpu::init_secondary(cpu_id);

    // Initialize trap, console, time.
    axplat::init::init_early_secondary(cpu_id);

    // Initialize platform peripherals, such as IRQ handlers.
    axplat::init::init_later_secondary(cpu_id);
}
