//! Interrupt management.

use ax_cpu::trap::set_irq_handler;
#[cfg(feature = "smp")]
pub use ax_plat::irq::init_secondary_boot_irqs;
pub use ax_plat::irq::{
    AARCH64_GIC_DOMAIN, AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger,
    AutoEnable, BoxedIrqHandler, CPU_LOCAL_IRQ_DOMAIN, CpuId, CpuMask, HwIrq, IrqAffinity,
    IrqContext, IrqDomainId, IrqError, IrqExecution, IrqHandle, IrqId, IrqNumber, IrqOutcome,
    IrqRequest, IrqReturn, IrqScope, IrqSource, IrqStatus, IrqTrigger, LEGACY_IRQ_DOMAIN,
    LOONGARCH_EIOINTC_DOMAIN, LOONGARCH_PCH_PIC_DOMAIN, RISCV_PLIC_DOMAIN, ShareMode, TrapVector,
    X86_IOAPIC_DOMAIN, X86_LAPIC_DOMAIN, cpu_online, disable_irq, dispatch_irq, enable_irq,
    free_irq, in_irq_context, init_boot_irqs, irq_status, is_cpu_online, legacy_irq,
    legacy_irq_raw, request_irq, request_percpu_irq, request_shared_irq, resolve_irq_source,
    resolve_percpu_irq, run_on_cpu_sync, set_enable, set_run_on_cpu_sync, set_trigger,
    synchronize_irq, try_legacy_irq,
};
#[cfg(feature = "ipi")]
pub use ax_plat::irq::{IpiTarget, send_ipi};
use ax_plat::irq::{handle, prepare_irq_context};

/// Returns the platform IRQ id used for inter-processor interrupts.
#[cfg(feature = "ipi")]
pub fn ipi_irq() -> IrqId {
    ax_plat::irq::ipi_irq()
}

/// IRQ handler.
///
/// Normalizes both hardware-trap and hypervisor VM-exit callers to the same
/// local-IRQ-disabled entry contract. A hypervisor may restore the host IRQ
/// state before forwarding a deferred external interrupt.
///
/// # Warning
///
/// Make sure called in an interrupt context or hypervisor VM exit handler.
pub fn handle_irq(vector: usize) -> bool {
    with_irq_entry(
        || prepare_irq_context(TrapVector(vector)),
        || handle(TrapVector(vector)).is_some(),
    )
}

/// Dispatches an IRQ that was acknowledged by an architecture backend and
/// completes its controller token before IRQ-return scheduling.
///
/// Hypervisors may consume the physical interrupt token while the guest is
/// running and dispatch the already-resolved action only after dropping guest
/// CPU ownership. This entry retains the same IRQ/preemption contract as a
/// hardware trap without acknowledging the controller a second time.
/// `complete` must finish the matching controller transaction without sleeping
/// or enabling local IRQs.
pub fn handle_acknowledged_irq(irq: IrqId, complete: impl FnOnce()) -> IrqOutcome {
    with_irq_entry_and_completion(|| {}, || dispatch_irq(irq), complete)
}

fn with_irq_entry<T>(prepare: impl FnOnce(), dispatch: impl FnOnce() -> T) -> T {
    with_irq_entry_and_completion(prepare, dispatch, || {})
}

fn with_irq_entry_and_completion<T>(
    prepare: impl FnOnce(),
    dispatch: impl FnOnce() -> T,
    complete: impl FnOnce(),
) -> T {
    with_observed_irq_entry(prepare, dispatch, complete, || {})
}

fn with_observed_irq_entry<T>(
    prepare: impl FnOnce(),
    dispatch: impl FnOnce() -> T,
    complete: impl FnOnce(),
    after_preempt_release: impl FnOnce(),
) -> T {
    let mut irq_guard = ax_sync::IrqSaveGuard::new();
    prepare();
    let preempt_guard = irq_guard.disable_preempt_for_irq_return();
    ax_sync::hardirq_enter();
    let result = dispatch();
    ax_sync::hardirq_exit();

    finish_irq_entry(|| drop(preempt_guard), complete);
    after_preempt_release();
    drop(irq_guard);
    result
}

fn finish_irq_entry(release_preempt: impl FnOnce(), complete: impl FnOnce()) {
    complete();
    release_preempt(); // Explicit IRQ-return scheduling keeps local IRQs disabled.
}

/// Tests IRQ-action context while the caller already pins the current CPU.
///
/// # Safety
///
/// The caller must prevent migration for the complete CPU identity and IRQ
/// publication observation.
#[doc(hidden)]
#[inline(always)]
pub unsafe fn in_irq_context_pinned() -> bool {
    // SAFETY: forwarded caller contract prevents migration for this complete
    // non-escaping CPU-area observation.
    let cpu = unsafe { cpu_local::with_cpu_pin(|pin| pin.area().cpu_index().as_usize()) }
        .map(CpuId)
        .unwrap_or_else(|error| panic!("IRQ context CPU identity is invalid: {error}"));
    ax_plat::irq::in_irq_context_on(cpu)
}

/// Installs the default ArceOS IRQ dispatcher into `ax-cpu`'s runtime hook.
///
/// This is intended for runtimes that dispatch traps through
/// [`ax_cpu::trap::dispatch_irq`] instead of relying on the `#[irq_handler]`
/// link-time override path.
pub fn init_common_irq_handler() {
    let _ = set_irq_handler(handle_irq);
}

#[cfg(axtest)]
pub(crate) struct IrqEntryStateObservation {
    pub(crate) dispatch_irqs_enabled: bool,
    pub(crate) after_preempt_release_irqs_enabled: bool,
    pub(crate) return_irqs_enabled: bool,
}

#[cfg(axtest)]
pub(crate) fn observe_irq_entry_state_for_test() -> IrqEntryStateObservation {
    let mut after_preempt_release_irqs_enabled = false;
    let dispatch_irqs_enabled = with_observed_irq_entry(
        || {},
        crate::asm::irqs_enabled,
        || {},
        || after_preempt_release_irqs_enabled = crate::asm::irqs_enabled(),
    );

    IrqEntryStateObservation {
        dispatch_irqs_enabled,
        after_preempt_release_irqs_enabled,
        return_irqs_enabled: crate::asm::irqs_enabled(),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{cell::RefCell, vec::Vec};

    use super::finish_irq_entry;

    #[test]
    fn acknowledged_irq_completion_precedes_preempt_release() {
        let events = RefCell::new(Vec::new());

        finish_irq_entry(
            || events.borrow_mut().push("preempt-release"),
            || events.borrow_mut().push("controller-complete"),
        );

        assert_eq!(
            *events.borrow(),
            ["controller-complete", "preempt-release"],
            "an acknowledged controller token must not remain active across IRQ-return scheduling",
        );
    }
}
