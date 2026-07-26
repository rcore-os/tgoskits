//! CPU-local data structures and accessors.

#[ax_percpu::def_percpu]
static IS_BSP: bool = false;

/// Returns the ID of the current CPU.
#[inline]
pub fn this_cpu_id() -> usize {
    let _guard = ax_kernel_guard::NoPreempt::new();
    // SAFETY: NoPreempt prevents migration until the pinned lookup returns.
    unsafe { ax_percpu::with_cpu_pin(this_cpu_id_pinned) }
        .expect("the current CPU-local area must remain bound")
}

/// Returns the current CPU ID while the caller's pin remains active.
#[inline]
pub fn this_cpu_id_pinned(pin: &ax_percpu::CpuPin<'_>) -> usize {
    ax_percpu::current_cpu_index(pin).as_usize()
}

/// Returns whether the current CPU is the primary CPU (aka the bootstrap
/// processor or BSP).
#[inline]
pub fn this_cpu_is_bsp() -> bool {
    let _guard = ax_kernel_guard::NoPreempt::new();
    // SAFETY: NoPreempt prevents migration until the pinned lookup returns.
    unsafe { ax_percpu::with_cpu_pin(this_cpu_is_bsp_pinned) }
        .expect("the current CPU-local area must remain bound")
}

/// Returns whether the pinned CPU is the bootstrap processor.
#[inline]
pub fn this_cpu_is_bsp_pinned(pin: &ax_percpu::CpuPin<'_>) -> bool {
    IS_BSP.read_current(pin)
}

/// Initializes CPU-local data structures for the primary core.
///
/// This function should be called as early as possible, as other
/// initializations may access the CPU-local data.
pub fn init_primary(cpu_id: usize) {
    verify_platform_binding(cpu_id);
    // SAFETY: primary runtime initialization is serialized before IRQs and
    // scheduling can expose this CPU-local flag.
    unsafe { ax_percpu::with_cpu_pin(|pin| IS_BSP.write_current(pin, true)) }
        .expect("primary CPU-local area must be installed");
}

/// Initializes CPU-local data structures for a secondary core.
///
/// This function should be called as early as possible, as other
/// initializations may access the CPU-local data.
#[cfg(feature = "smp")]
pub fn init_secondary(cpu_id: usize) {
    verify_platform_binding(cpu_id);
    // SAFETY: secondary runtime initialization is serialized before the CPU is
    // published online.
    unsafe { ax_percpu::with_cpu_pin(|pin| IS_BSP.write_current(pin, false)) }
        .expect("secondary CPU-local area must be installed");
}

fn verify_platform_binding(cpu_id: usize) {
    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_id)
        .expect("logical CPU index must fit the CPU-local range");
    let expected = ax_percpu::area(cpu_index)
        .expect("the selected platform must install its CPU-local layout before ax-runtime");
    // SAFETY: runtime per-CPU initialization precedes scheduler publication and
    // IRQ enablement, so this CPU cannot migrate for the verification window.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            let actual = ax_percpu::current_area(pin)
                .expect("the selected platform must expose the current CPU area");
            assert_eq!(
                actual, expected,
                "the selected platform bound the wrong CPU-local area"
            );
        })
    }
    .expect("the selected platform must install its CPU-local area");
}
