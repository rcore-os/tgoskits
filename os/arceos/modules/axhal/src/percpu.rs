//! Typed facade for CPU-local area and scheduler publication capabilities.

use core::{pin::Pin, ptr::NonNull};

#[cfg(feature = "smp")]
pub use ax_plat::percpu::init_secondary;
pub use ax_plat::percpu::{
    init_primary, this_cpu_id, this_cpu_id_pinned, this_cpu_is_bsp, this_cpu_is_bsp_pinned,
};
pub use cpu_local::{
    CpuAreaRef, CpuLocalError, CpuPin, CurrentContext, CurrentThreadHeader, ExclusiveCpu,
    PreemptExit, PreemptGuardOwner, PreparedThreadSwitch, PreviousThreadBinding,
    RuntimeThreadCookie, ThreadSwitchError, with_cpu_pin, with_exclusive_cpu,
};
#[cfg(feature = "task-test-hooks")]
#[doc(hidden)]
pub use cpu_local::{
    reset_preempt_guard_owner_resolution_count, take_preempt_guard_owner_resolution_count,
};

#[inline(always)]
fn with_scheduler_current_exclusion<R>(
    requires_exclusion: bool,
    irqs_enabled: impl FnOnce() -> bool,
    disable_irqs: impl FnOnce(),
    enable_irqs: impl FnOnce(),
    operation: impl FnOnce() -> R,
) -> R {
    let restore_irqs = requires_exclusion && irqs_enabled();
    if restore_irqs {
        disable_irqs();
    }
    let result = operation();
    if restore_irqs {
        enable_irqs();
    }
    result
}

#[inline(always)]
fn with_stable_scheduler_current<R>(operation: impl FnOnce() -> R) -> R {
    with_scheduler_current_exclusion(
        cpu_local::scheduler_current_requires_irq_exclusion(),
        crate::asm::irqs_enabled,
        crate::asm::disable_irqs,
        crate::asm::enable_irqs,
        operation,
    )
}

/// Reads ordinary preemption nesting from the stable current task.
#[inline(always)]
pub fn scheduler_preempt_guard_depth() -> Result<u32, CpuLocalError> {
    with_stable_scheduler_current(cpu_local::scheduler_preempt_guard_depth)
}

/// Reads ordinary preemption nesting from a live guard owner.
#[inline(always)]
pub fn scheduler_owned_preempt_guard_depth(owner: PreemptGuardOwner) -> u32 {
    cpu_local::scheduler_owned_preempt_guard_depth(owner)
}

/// Publishes scheduler work into the stable current task.
#[inline(always)]
pub fn scheduler_set_preempt_need_resched() -> Result<(), CpuLocalError> {
    with_stable_scheduler_current(cpu_local::scheduler_set_preempt_need_resched)
}

/// Clears scheduler work from the stable current task.
#[inline(always)]
pub fn scheduler_clear_preempt_need_resched() -> Result<(), CpuLocalError> {
    with_stable_scheduler_current(cpu_local::scheduler_clear_preempt_need_resched)
}

/// Enters one preemption guard on the stable current task.
#[inline(always)]
pub fn scheduler_enter_preempt_guard() -> Result<PreemptGuardOwner, CpuLocalError> {
    with_stable_scheduler_current(cpu_local::scheduler_enter_preempt_guard)
}

/// Prepares one preemption-guard exit on the stable current task.
#[inline(always)]
pub fn scheduler_prepare_preempt_guard_exit(owner: PreemptGuardOwner) -> PreemptExit {
    cpu_local::scheduler_prepare_preempt_guard_exit(owner)
}

/// Consumes the final guard retained by the stable current task.
#[inline(always)]
pub fn scheduler_consume_final_preempt_guard(owner: PreemptGuardOwner) -> bool {
    cpu_local::scheduler_consume_final_preempt_guard(owner)
}

/// Resolves the owner of an already-live ordinary preemption guard.
///
/// # Safety
///
/// The caller must retain the live guard depth until the returned owner is
/// consumed by the matching exit path.
#[inline(always)]
pub unsafe fn scheduler_current_preempt_guard_owner() -> Result<PreemptGuardOwner, CpuLocalError> {
    with_stable_scheduler_current(|| unsafe { cpu_local::scheduler_current_preempt_guard_owner() })
}

/// Returns the direct current CPU-area base under an explicit pin.
pub fn cpu_base(pin: &CpuPin<'_>) -> NonNull<u8> {
    // CpuPin construction already validated the non-null initialized area.
    unsafe { NonNull::new_unchecked(pin.area().base() as *mut u8) }
}

/// Returns the validated current CPU area.
pub const fn current_cpu_area(pin: &CpuPin<'_>) -> CpuAreaRef {
    pin.area()
}

/// Returns the pinned current execution-context header.
pub fn current_thread(pin: &CpuPin<'_>) -> Result<NonNull<CurrentThreadHeader>, CpuLocalError> {
    cpu_local::current_thread(pin)
}

/// Reads current-thread identity before constructing a scheduler guard.
///
/// # Safety
///
/// The caller must keep the scheduler-owned current task alive and must not
/// dereference the result after a context switch.
pub unsafe fn scheduler_current_thread_unpinned()
-> Result<NonNull<CurrentThreadHeader>, CpuLocalError> {
    with_stable_scheduler_current(|| unsafe { cpu_local::scheduler_current_thread() })
}

/// Runs `f` with the task-owned header selected by the architecture `current`
/// source without pinning the task to a CPU.
#[inline(always)]
pub fn with_scheduler_current_thread<R>(
    f: impl for<'current> FnOnce(&'current CurrentThreadHeader) -> R,
) -> Result<R, CpuLocalError> {
    with_stable_scheduler_current(|| cpu_local::with_scheduler_current_thread(f))
}

/// Reads the logical CPU ID before constructing a scheduler guard.
///
/// # Safety
///
/// The caller must already prevent migration or own an offline CPU and must
/// not use this observation after a context switch.
#[doc(hidden)]
#[inline(always)]
pub unsafe fn scheduler_current_cpu_id() -> usize {
    unsafe { cpu_local::scheduler_current_cpu_index() }
        .expect("scheduler current thread must retain a CPU binding")
        .as_usize()
}

/// Prepares a complete current-thread switch transaction.
///
/// # Safety
///
/// The caller must own the IRQ-disabled scheduler path and keep both task
/// allocations pinned through the raw switch and incoming tail.
pub unsafe fn prepare_thread_switch<'switch>(
    pin: &'switch CpuPin<'_>,
    previous: Pin<&CurrentThreadHeader>,
    next: Pin<&CurrentThreadHeader>,
) -> Result<(PreparedThreadSwitch<'switch>, PreviousThreadBinding), ThreadSwitchError> {
    unsafe { cpu_local::prepare_thread_switch(pin, previous, next) }
}

/// Installs the scheduler bootstrap task on an offline CPU.
///
/// # Safety
///
/// The CPU must be offline and trap-free, and `header` must remain pinned.
pub unsafe fn install_bootstrap_thread(
    pin: &CpuPin<'_>,
    header: Pin<&CurrentThreadHeader>,
) -> Result<(), ThreadSwitchError> {
    unsafe { cpu_local::install_bootstrap_thread(pin, header) }
}

/// Reads the current task-owned kernel TLS base.
#[cfg(feature = "tls")]
pub fn kernel_tls(pin: &CpuPin<'_>) -> crate::context::KernelTlsBase {
    crate::context::KernelTlsBase::new(cpu_local::kernel_tls(pin))
}

/// Installs bootstrap task TLS before scheduling starts.
///
/// # Safety
///
/// The CPU must remain offline, and `kernel_tls` must remain valid while the
/// bootstrap context executes.
#[cfg(feature = "tls")]
pub unsafe fn install_bootstrap_kernel_tls(
    pin: &CpuPin<'_>,
    kernel_tls: crate::context::KernelTlsBase,
) {
    unsafe { cpu_local::install_kernel_tls(pin, kernel_tls.as_usize()) };
}

/// Allocates and installs CPU zero for host-side scheduler tests.
#[cfg(feature = "host-test")]
pub fn initialize_host_test_cpu() {
    use core::num::NonZeroU32;

    let layout = ax_percpu::host_test::initialize(NonZeroU32::new(1).unwrap())
        .expect("host per-CPU layout must initialize");
    let cpu_index = ax_percpu::CpuIndex::try_from(0).expect("CPU zero must be representable");
    let area = layout
        .area(cpu_index)
        .expect("host CPU zero area must exist");
    let cpu_area = area.cpu_area().expect("host CPU zero prefix must be valid");

    // SAFETY: the scheduler test worker models one offline, non-migrating CPU
    // and the process-lifetime fixture is fully initialized.
    match unsafe { cpu_local::install_cpu_area(cpu_area) } {
        Ok(()) => {}
        Err(CpuLocalError::AreaNotInstalled) => unreachable!(),
        Err(error) => {
            // Repeated host initialization is accepted when this thread
            // already has the same area installed.
            let current = unsafe { cpu_local::with_cpu_pin(|pin| pin.area()) };
            assert_eq!(
                current,
                Ok(cpu_area),
                "invalid host CPU-local state: {error}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::with_scheduler_current_exclusion;

    #[test]
    fn cpu_anchor_current_is_observed_under_local_irq_exclusion() {
        let irq_enabled = Cell::new(true);
        with_scheduler_current_exclusion(
            true,
            || irq_enabled.get(),
            || irq_enabled.set(false),
            || irq_enabled.set(true),
            || {
                assert!(
                    !irq_enabled.get(),
                    "CPU-anchor current may migrate while sampled"
                )
            },
        );
        assert!(irq_enabled.get(), "the caller's IRQ state must be restored");
    }

    #[test]
    fn architecture_current_does_not_mutate_local_irq_state() {
        let irq_enabled = Cell::new(true);
        with_scheduler_current_exclusion(
            false,
            || irq_enabled.get(),
            || panic!("an architecture current register needs no IRQ exclusion"),
            || panic!("an architecture current register needs no IRQ restore"),
            || assert!(irq_enabled.get()),
        );
    }
}
