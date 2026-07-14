//! CPU-local data structures and accessors.

#[ax_percpu::def_percpu]
static IS_BSP: bool = false;

/// Returns the ID of the current CPU.
#[inline]
pub fn this_cpu_id() -> usize {
    let guard = ax_kspin::PreemptGuard::new();
    this_cpu_id_pinned(guard.cpu_pin())
}

/// Returns the current CPU ID while the caller's pin remains active.
#[inline]
pub fn this_cpu_id_pinned(pin: &ax_percpu::CpuPin) -> usize {
    let bound_pin =
        ax_percpu::bound_current(pin).expect("the current CPU-local area must remain bound");
    ax_percpu::current_cpu_index(&bound_pin)
        .expect("the current CPU-local area must remain bound")
        .as_usize()
}

/// Returns whether the current CPU is the primary CPU (aka the bootstrap
/// processor or BSP)
#[inline]
pub fn this_cpu_is_bsp() -> bool {
    let guard = ax_kspin::PreemptGuard::new();
    this_cpu_is_bsp_pinned(guard.cpu_pin())
}

/// Returns whether the pinned CPU is the bootstrap processor.
#[inline]
pub fn this_cpu_is_bsp_pinned(pin: &ax_percpu::CpuPin) -> bool {
    let bound_pin =
        ax_percpu::bound_current(pin).expect("the current CPU-local area must remain bound");
    IS_BSP.read_current(&bound_pin)
}

/// Initializes CPU-local data structures for the primary core.
///
/// This function should be called as early as possible, as other
/// initializations may access the CPU-local data.
pub fn init_primary(cpu_id: usize) {
    verify_platform_binding(cpu_id);
    unsafe {
        IS_BSP.write_current_raw(true);
    }
}

/// Initializes CPU-local data structures for secondary cores.
///
/// This function should be called as early as possible, as other
/// initializations may access the CPU-local data.
#[cfg(feature = "smp")]
pub fn init_secondary(cpu_id: usize) {
    verify_platform_binding(cpu_id);
    unsafe {
        IS_BSP.write_current_raw(false);
    }
}

fn verify_platform_binding(cpu_id: usize) {
    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_id)
        .expect("logical CPU index must fit the CPU-local ABI");
    let area = ax_percpu::area(cpu_index)
        .expect("the selected platform must install its CPU-local layout before ax-runtime");
    // SAFETY: runtime per-CPU initialization precedes scheduler publication and
    // IRQ enablement, so this CPU cannot migrate for the verification window.
    let pin = unsafe { ax_percpu::CpuPin::new_unchecked() };
    // SAFETY: the selected platform bound this same CPU-lifetime area before
    // transferring control to ax-runtime.
    unsafe { ax_percpu::verify_current(area, &pin) }
        .expect("the selected platform bound the wrong CPU-local area");
}
