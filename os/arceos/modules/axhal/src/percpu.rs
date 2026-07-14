//! CPU-local data structures.

pub use ax_plat::percpu::*;

#[ax_percpu::def_percpu]
static CURRENT_TASK_PTR: usize = 0;

/// Gets the pointer to the current task with preemption-safety.
///
/// Preemption may be enabled when calling this function. This function will
/// guarantee the correctness even the current task is preempted.
#[inline]
pub fn current_task_ptr<T>() -> *const T {
    // The architecture-independent per-CPU path reads the CPU anchor and then
    // calculates the symbol address. Keep that complete sequence on one CPU on
    // every architecture, including x86 where no direct GS fast path is exposed.
    let guard = ax_kspin::IrqGuard::new();
    let cpu_pin = ax_percpu::bound_current(guard.cpu_pin())
        .expect("current task access requires a bound CPU-local area");
    CURRENT_TASK_PTR.read_current(&cpu_pin) as _
}

/// Sets the pointer to the current task with preemption-safety.
///
/// Preemption may be enabled when calling this function. This function will
/// guarantee the correctness even the current task is preempted.
///
/// # Safety
///
/// The given `ptr` must be pointed to a valid task structure.
#[inline]
pub unsafe fn set_current_task_ptr<T>(ptr: *const T) {
    let guard = ax_kspin::IrqGuard::new();
    let cpu_pin = ax_percpu::bound_current(guard.cpu_pin())
        .expect("current task access requires a bound CPU-local area");
    CURRENT_TASK_PTR.write_current(&cpu_pin, ptr as usize);
}
