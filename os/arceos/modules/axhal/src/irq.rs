//! Interrupt management.

use ax_cpu::trap::set_irq_handler;
#[cfg(feature = "smp")]
pub use ax_plat::irq::init_secondary_boot_irqs;
pub use ax_plat::irq::{
    AARCH64_GIC_DOMAIN, AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger,
    AutoEnable, BoxedIrqHandler, CPU_LOCAL_IRQ_DOMAIN, CpuId, CpuMask, HwIrq, IrqAffinity,
    IrqContext, IrqDomainId, IrqError, IrqExecution, IrqHandle, IrqId, IrqNumber, IrqOutcome,
    IrqRequest, IrqReturn, IrqScope, IrqSource, IrqStatus, LEGACY_IRQ_DOMAIN,
    LOONGARCH_EIOINTC_DOMAIN, LOONGARCH_PCH_PIC_DOMAIN, RISCV_PLIC_DOMAIN, ShareMode, TrapVector,
    X86_IOAPIC_DOMAIN, X86_LAPIC_DOMAIN, cpu_online, disable_irq, dispatch_irq, enable_irq,
    free_irq, handle, in_irq_context, init_boot_irqs, irq_status, legacy_irq, legacy_irq_raw,
    prepare_irq_context, request_irq, request_percpu_irq, request_shared_irq, resolve_irq_source,
    resolve_percpu_irq, run_on_cpu_sync, set_enable, set_run_on_cpu_sync, synchronize_irq,
    try_legacy_irq,
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
/// # Warn
///
/// Make sure called in an interrupt context or hypervisor VM exit handler.
pub fn handle_irq(vector: usize) -> bool {
    prepare_irq_context(TrapVector(vector));
    let guard = ax_kernel_guard::NoPreempt::new();
    let handled = handle(TrapVector(vector)).is_some();

    drop(guard); // rescheduling may occur when preemption is re-enabled.
    handled
}

/// Installs the default ArceOS IRQ dispatcher into `ax-cpu`'s runtime hook.
///
/// This is intended for runtimes that dispatch traps through
/// [`ax_cpu::trap::dispatch_irq`] instead of relying on the `#[irq_handler]`
/// link-time override path.
pub fn init_common_irq_handler() {
    let _ = set_irq_handler(handle_irq);
}
