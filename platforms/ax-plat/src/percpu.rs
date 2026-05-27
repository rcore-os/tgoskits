//! CPU-local data structures and accessors.

#[ax_percpu::def_percpu]
static CPU_ID: usize = 0;

#[ax_percpu::def_percpu]
static IS_BSP: bool = false;

/// Returns the ID of the current CPU.
#[inline]
pub fn this_cpu_id() -> usize {
    CPU_ID.read_current()
}

/// Returns whether the current CPU is the primary CPU (aka the bootstrap
/// processor or BSP)
#[inline]
pub fn this_cpu_is_bsp() -> bool {
    IS_BSP.read_current()
}

/// Initializes CPU-local data structures for the primary core.
///
/// This function should be called as early as possible, as other
/// initializations may access the CPU-local data.
pub fn init_primary(cpu_id: usize) {
    ax_percpu::init();
    ax_percpu::init_percpu_reg(cpu_id);
    unsafe {
        CPU_ID.write_current_raw(cpu_id);
        IS_BSP.write_current_raw(true);
    }
}

/// Initializes CPU-local data structures for secondary cores.
///
/// This function should be called as early as possible, as other
/// initializations may access the CPU-local data.
#[cfg(feature = "smp")]
pub fn init_secondary(cpu_id: usize) {
    ax_percpu::init_percpu_reg(cpu_id);
    unsafe {
        CPU_ID.write_current_raw(cpu_id);
        IS_BSP.write_current_raw(false);
    }
}
