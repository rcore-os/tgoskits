use core::sync::atomic::{AtomicUsize, Ordering::Acquire};

use crate::CPU_NUM;

/// Number of CPUs finished initialization.
pub static INITED_CPUS: AtomicUsize = AtomicUsize::new(0);
pub fn init_smp_ok() -> bool {
    INITED_CPUS.load(Acquire) == CPU_NUM
}

pub fn init_kernel(cpu_id: usize, arg: usize) {
    percpu::init();
    percpu::init_percpu_reg(cpu_id);
    init_cpu_id(cpu_id);

    // Initialize trap, console, time.
    axplat::init::init_early(cpu_id, arg);

    // Initialize platform peripherals, such as IRQ handlers.
    axplat::init::init_later(cpu_id, arg);
}

pub fn init_kernel_secondary(cpu_id: usize) {
    percpu::init_percpu_reg(cpu_id);
    init_cpu_id(cpu_id);

    // Initialize trap, console, time.
    axplat::init::init_early_secondary(cpu_id);

    // Initialize platform peripherals, such as IRQ handlers.
    axplat::init::init_later_secondary(cpu_id);
}

#[percpu::def_percpu]
static CPU_ID: usize = 0;

pub fn this_cpu_id() -> usize {
    unsafe { CPU_ID.read_current_raw() }
}

/// Initialize the CPU ID for the current thread.
pub fn init_cpu_id(cpu_id: usize) {
    unsafe {
        CPU_ID.write_current_raw(cpu_id);
    }
}
