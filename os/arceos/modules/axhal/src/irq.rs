//! Interrupt management.

use core::marker::PhantomData;

use ax_cpu::trap::{TrapIrqPermit, set_irq_handler};
use ax_cpu_local::CpuPin;
use ax_kspin::{IrqGuard, PreemptGuard};
#[cfg(feature = "smp")]
pub use ax_plat::irq::init_secondary_boot_irqs;
pub use ax_plat::irq::{
    AARCH64_GIC_DOMAIN, AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger,
    AutoEnable, BoxedIrqHandler, CPU_LOCAL_IRQ_DOMAIN, CpuId, CpuMask, DetachedIrqAction, HwIrq,
    IrqAffinity, IrqContext, IrqContinuationSlot, IrqContinuationToken, IrqContinuationWake,
    IrqDomainId, IrqDrainToken, IrqDrainWake, IrqError, IrqExecution, IrqHandle, IrqId, IrqNumber,
    IrqOutcome, IrqRequest, IrqReturn, IrqScope, IrqSource, IrqStatus, LEGACY_IRQ_DOMAIN,
    LOONGARCH_EIOINTC_DOMAIN, LOONGARCH_PCH_PIC_DOMAIN, RISCV_PLIC_DOMAIN,
    ReattachIrqActionError, ShareMode, TrapVector, X86_IOAPIC_DOMAIN, X86_LAPIC_DOMAIN, cpu_online,
    detach_irq_action, disable_irq, disable_irq_async, dispatch_irq, enable_irq,
    finish_irq_continuation, free_irq, handle, in_irq_context, init_boot_irqs,
    irq_action_drain_complete, irq_status, legacy_irq, legacy_irq_raw, prepare_irq_context,
    reattach_irq_action, release_irq_quench, request_irq, request_percpu_irq, request_shared_irq,
    resolve_irq_source, resolve_percpu_irq, run_on_cpu_sync, set_enable, set_run_on_cpu_sync,
    synchronize_irq, try_legacy_irq,
};
#[cfg(feature = "ipi")]
pub use ax_plat::irq::{CpuIpiTarget, IpiSendStatus, send_ipi};

/// Returns the platform IRQ id used for inter-processor interrupts.
#[cfg(feature = "ipi")]
pub fn ipi_irq() -> IrqId {
    ax_plat::irq::ipi_irq()
}

/// Linear proof for a masked host IRQ retained across vCPU unbind.
///
/// Unlike [`TrapIrqPermit`], this permit is not tied to an architecture trap
/// return. It borrows the outer CPU pin which covers bind, guest entry, host
/// register restoration, unbind, and this final controller transaction. It is
/// neither cloneable nor transferable to another CPU.
#[must_use = "a pinned host IRQ permit must be consumed before restoring its saved IRQ state"]
#[derive(Debug)]
pub struct PinnedHostIrqPermit<'pin> {
    vector: usize,
    cpu_pin: &'pin CpuPin,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl<'pin> PinnedHostIrqPermit<'pin> {
    /// Transfers one pending host IRQ into the post-unbind dispatcher.
    ///
    /// # Safety
    ///
    /// The caller must have restored all host register anchors, unbound the
    /// vCPU, and cleared its CPU-local current-vCPU publication while retaining
    /// the migration guard represented by `cpu_pin`. Raw local IRQs must remain
    /// masked by the unique saved host IRQ state, which the caller restores
    /// only after this permit is consumed. `vector` must identify the pending
    /// architecture cause; platforms such as AArch64 may ignore it and claim
    /// the real controller interrupt.
    pub unsafe fn from_post_unbind(vector: usize, cpu_pin: &'pin CpuPin) -> Self {
        Self {
            vector,
            cpu_pin,
            _not_send_or_sync: PhantomData,
        }
    }

    fn into_parts(self) -> (usize, &'pin CpuPin) {
        (self.vector, self.cpu_pin)
    }
}

fn assert_trap_irqs_masked(stage: &str) {
    let masked = !crate::asm::irqs_enabled();
    if !masked {
        // Keep a broken trap callback fail-closed before reporting the
        // ownership violation. The saved trap frame remains the only object
        // allowed to restore the interrupted IRQ state.
        crate::asm::disable_irqs();
    }
    assert!(masked, "raw IRQs became enabled during {stage}");
}

/// Dispatches an IRQ owned by an architecture trap-return continuation.
///
/// The linear permit, rather than the callback's final IRQ mask, selects the
/// IRQ-return scheduler completion. Raw IRQs must remain masked throughout;
/// the architecture trap frame restores the interrupted state afterwards.
pub fn handle_trap_irq(permit: TrapIrqPermit) -> bool {
    let vector = permit.vector();
    assert_trap_irqs_masked("trap IRQ entry");
    prepare_irq_context(TrapVector(vector));
    let guard = PreemptGuard::new();
    let handled = handle(TrapVector(vector)).is_some();
    assert_trap_irqs_masked("trap IRQ dispatch");

    unsafe {
        // SAFETY: dispatch completed controller EOI and removed the hard-IRQ
        // marker. `TrapIrqPermit` proves that an architecture continuation
        // still owns restoration of the interrupted raw IRQ state.
        guard.finish_irq_return();
    }
    assert_trap_irqs_masked("trap IRQ return");
    handled
}

/// Claims and dispatches a host IRQ after vCPU unbind while IRQs stay masked.
///
/// The consumed permit proves that an outer migration guard and saved raw IRQ
/// state remain live. This function deliberately creates neither an IRQ guard
/// nor a trap-return scheduler baton: controller completion happens here, then
/// the caller restores its exact saved IRQ state and finally releases the outer
/// preemption guard.
pub fn handle_pinned_host_irq(permit: PinnedHostIrqPermit<'_>) -> bool {
    let (vector, _cpu_pin) = permit.into_parts();
    assert_trap_irqs_masked("pinned host IRQ entry");
    prepare_irq_context(TrapVector(vector));
    let handled = handle(TrapVector(vector)).is_some();
    assert_trap_irqs_masked("pinned host IRQ dispatch");
    handled
}

/// Claims and dispatches a pending IRQ from ordinary task or deferred VM-exit work.
///
/// This path owns its raw IRQ mask through [`IrqGuard`] and performs an ordinary
/// preemption exit after restoring that saved task-context mask. It must never
/// borrow the trap-return completion path merely because IRQs happen to be
/// masked on entry.
pub fn handle_irq_from_task(vector: usize) -> bool {
    assert!(
        crate::asm::irqs_enabled(),
        "task IRQ dispatch entered before the host IRQ state was restored"
    );
    let preempt_guard = PreemptGuard::new();
    let irq_guard = IrqGuard::new();
    prepare_irq_context(TrapVector(vector));
    let handled = handle(TrapVector(vector)).is_some();
    assert_trap_irqs_masked("task IRQ dispatch");

    drop(irq_guard);
    assert!(
        crate::asm::irqs_enabled(),
        "task IRQ guard failed to restore the enabled host IRQ state"
    );
    drop(preempt_guard);
    handled
}

/// Installs the default ArceOS IRQ dispatcher into `ax-cpu`'s runtime hook.
///
/// This is intended for runtimes that dispatch traps through
/// [`ax_cpu::trap::dispatch_irq`] instead of relying on the `#[irq_handler]`
/// link-time override path.
pub fn init_common_irq_handler() {
    let _ = set_irq_handler(handle_trap_irq);
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! assert_not_impl {
        ($tested_type:ty, $tested_trait:path) => {
            const _: fn() = || {
                trait AmbiguousIfImplemented<Marker> {
                    fn check() {}
                }

                impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}

                struct Implemented;
                impl<T: ?Sized + $tested_trait> AmbiguousIfImplemented<Implemented> for T {}

                let _ = <$tested_type as AmbiguousIfImplemented<_>>::check;
            };
        };
    }

    assert_not_impl!(PinnedHostIrqPermit<'static>, Send);
    assert_not_impl!(PinnedHostIrqPermit<'static>, Clone);
    assert_not_impl!(PinnedHostIrqPermit<'static>, Copy);
}
