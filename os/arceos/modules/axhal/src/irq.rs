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
    enter_irq_context, free_irq, handle, in_irq_context, in_irq_context_preempt_disabled,
    init_boot_irqs, irq_status, is_cpu_online, legacy_irq, legacy_irq_raw, prepare_irq_context,
    request_irq, request_percpu_irq, request_shared_irq, resolve_irq_source, resolve_percpu_irq,
    run_on_cpu_sync, set_enable, set_run_on_cpu_sync, set_trigger, synchronize_irq, try_legacy_irq,
};
#[cfg(feature = "ipi")]
pub use ax_plat::irq::{IpiTarget, send_ipi};

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

/// Dispatches an IRQ whose controller acknowledgement is already owned by the
/// caller, then completes that acknowledgement before IRQ-tail preemption.
///
/// Hypervisor IRQ exits cannot call [`handle_irq`]: the GIC token was already
/// acknowledged before the architecture state was restored. This entry point
/// supplies the same IRQ-context and preemption-release ordering without a
/// second controller acknowledgement. `complete` must perform the matching
/// EOI/deactivate operation for the caller-owned token.
pub fn dispatch_acknowledged_irq(irq: IrqId, complete: impl FnOnce()) -> IrqOutcome {
    with_acknowledged_irq_entry(|| dispatch_irq(irq), complete, || {}, || {}, || {})
}

fn with_irq_entry<T>(prepare: impl FnOnce(), dispatch: impl FnOnce() -> T) -> T {
    with_observed_irq_entry(prepare, dispatch, || {}, || {}, || {})
}

fn with_acknowledged_irq_entry<T>(
    dispatch: impl FnOnce() -> T,
    complete: impl FnOnce(),
    after_irq_context_drop: impl FnOnce(),
    after_preempt_release: impl FnOnce(),
    after_irq_restore: impl FnOnce(),
) -> T {
    with_observed_irq_entry(
        || {},
        || {
            let result = dispatch();
            complete();
            result
        },
        after_irq_context_drop,
        after_preempt_release,
        after_irq_restore,
    )
}

fn with_observed_irq_entry<T>(
    prepare: impl FnOnce(),
    dispatch: impl FnOnce() -> T,
    after_irq_context_drop: impl FnOnce(),
    after_preempt_release: impl FnOnce(),
    after_irq_restore: impl FnOnce(),
) -> T {
    // Keep IRQs disabled until the preemption guard has handed any pending
    // reschedule back to the IRQ-return path. Hardware traps already enter in
    // this state; IrqSave also covers deferred VM-exit dispatchers.
    let irq_guard = ax_sync::IrqSaveGuard::new();
    prepare();
    let preempt_guard = ax_sync::PreemptGuard::new();
    let irq_context_guard = enter_irq_context();
    let result = dispatch();

    // A pending wakeup may turn into a context switch when preemption is
    // released. Withdraw the IRQ-context publication first so the incoming
    // task is never observed as executing inside the completed hard IRQ.
    drop(irq_context_guard);
    after_irq_context_drop();
    // The official scheduler-frame handoff owns the actual preemption release.
    // It may reschedule, but only after the acknowledged controller token has
    // been completed and the IRQ-context publication withdrawn.
    preempt_guard.finish_irq_return();
    after_preempt_release();
    drop(irq_guard);
    after_irq_restore();
    result
}

/// Installs the default ArceOS IRQ dispatcher into `ax-cpu`'s runtime hook.
///
/// This is intended for runtimes that dispatch traps through
/// [`ax_cpu::trap::dispatch_irq`] instead of relying on the `#[irq_handler]`
/// link-time override path.
pub fn init_common_irq_handler() {
    let _ = set_irq_handler(handle_irq);
}

#[cfg(all(axtest, feature = "axtest"))]
pub(crate) struct IrqEntryStateObservation {
    pub(crate) dispatch_irqs_enabled: bool,
    pub(crate) after_preempt_release_irqs_enabled: bool,
    pub(crate) return_irqs_enabled: bool,
}

#[cfg(all(axtest, feature = "axtest"))]
pub(crate) fn observe_irq_entry_state_for_test() -> IrqEntryStateObservation {
    let mut after_preempt_release_irqs_enabled = false;
    let dispatch_irqs_enabled = with_observed_irq_entry(
        || {},
        crate::asm::irqs_enabled,
        || {},
        || after_preempt_release_irqs_enabled = crate::asm::irqs_enabled(),
        || {},
    );

    IrqEntryStateObservation {
        dispatch_irqs_enabled,
        after_preempt_release_irqs_enabled,
        return_irqs_enabled: crate::asm::irqs_enabled(),
    }
}

#[cfg(all(axtest, feature = "axtest"))]
pub(crate) fn observe_acknowledged_irq_entry_order_for_test() -> [u8; 5] {
    use core::cell::RefCell;

    struct EventRecorder {
        events: [u8; 5],
        len: usize,
    }

    impl EventRecorder {
        fn push(&mut self, event: u8) {
            assert!(self.len < self.events.len(), "too many IRQ entry events");
            self.events[self.len] = event;
            self.len += 1;
        }
    }

    let recorder = RefCell::new(EventRecorder {
        events: [0; 5],
        len: 0,
    });
    with_acknowledged_irq_entry(
        || recorder.borrow_mut().push(1),
        || recorder.borrow_mut().push(2),
        || {
            assert!(
                !in_irq_context_preempt_disabled(),
                "IRQ context must be withdrawn before preemption release"
            );
            recorder.borrow_mut().push(3);
        },
        || recorder.borrow_mut().push(4),
        || recorder.borrow_mut().push(5),
    );
    recorder.into_inner().events
}
