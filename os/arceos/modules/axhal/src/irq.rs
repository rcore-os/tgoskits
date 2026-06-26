//! Interrupt management.

#[cfg(feature = "ipi")]
pub use ax_config::devices::IPI_IRQ;
use ax_cpu::trap::set_irq_handler;
pub use ax_plat::irq::{
    AARCH64_GIC_DOMAIN, AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger,
    AutoEnable, BoxedIrqHandler, CPU_LOCAL_IRQ_DOMAIN, CpuId, CpuMask, HwIrq, IrqAffinity,
    IrqContext, IrqDomainId, IrqError, IrqExecution, IrqHandle, IrqId, IrqNumber, IrqOutcome,
    IrqRequest, IrqReturn, IrqScope, IrqSource, IrqStatus, LEGACY_IRQ_DOMAIN,
    LOONGARCH_EIOINTC_DOMAIN, LOONGARCH_PCH_PIC_DOMAIN, RISCV_PLIC_DOMAIN, RawIrqHandler,
    ShareMode, TrapVector, X86_IOAPIC_DOMAIN, X86_LAPIC_DOMAIN, cpu_online, disable_irq,
    dispatch_irq, enable_irq, free_irq, handle, irq_status, legacy_irq, legacy_irq_raw,
    request_boxed_irq, request_boxed_shared_irq, request_irq, request_percpu_irq,
    request_shared_irq, resolve_irq_source, resolve_percpu_irq, run_on_cpu_sync, set_enable,
    set_run_on_cpu_sync, synchronize_irq, try_legacy_irq,
};
#[cfg(feature = "ipi")]
pub use ax_plat::irq::{IpiTarget, send_ipi};

/// Returns the platform IRQ id used for inter-processor interrupts.
///
/// `IPI_IRQ` is still an architecture/platform raw constant. Keep the conversion
/// here so IPI registration and delivery use the same IRQ domain.
#[cfg(all(feature = "ipi", plat_dyn, target_arch = "aarch64"))]
pub fn ipi_irq() -> IrqId {
    axplat_dyn::ipi_irq(IPI_IRQ as u32)
}

/// Returns the platform IRQ id used for inter-processor interrupts.
///
/// `IPI_IRQ` is still an architecture/platform raw constant. Keep the conversion
/// here so IPI registration and delivery use the same IRQ domain.
#[cfg(all(feature = "ipi", plat_dyn, target_arch = "riscv64"))]
pub fn ipi_irq() -> IrqId {
    const RISCV_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);

    if IPI_IRQ & RISCV_INTERRUPT_BIT != 0 {
        IrqId::new(
            CPU_LOCAL_IRQ_DOMAIN,
            HwIrq((IPI_IRQ & !RISCV_INTERRUPT_BIT) as u32),
        )
    } else {
        IrqNumber(IPI_IRQ).expect("IPI IRQ exceeds legacy IRQ width")
    }
}

/// Returns the platform IRQ id used for inter-processor interrupts.
///
/// `IPI_IRQ` is still an architecture/platform raw constant. Keep the conversion
/// here so IPI registration and delivery use the same IRQ domain.
#[cfg(all(
    feature = "ipi",
    plat_dyn,
    any(target_arch = "loongarch64", target_arch = "x86_64")
))]
pub fn ipi_irq() -> IrqId {
    IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(IPI_IRQ as u32))
}

/// Returns the platform IRQ id used for inter-processor interrupts.
#[cfg(all(
    feature = "ipi",
    not(all(
        plat_dyn,
        any(
            target_arch = "aarch64",
            target_arch = "loongarch64",
            target_arch = "riscv64",
            target_arch = "x86_64"
        )
    ))
))]
pub fn ipi_irq() -> IrqId {
    #[cfg(target_arch = "riscv64")]
    {
        const RISCV_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);
        if IPI_IRQ & RISCV_INTERRUPT_BIT != 0 {
            return IrqId::new(
                CPU_LOCAL_IRQ_DOMAIN,
                HwIrq((IPI_IRQ & !RISCV_INTERRUPT_BIT) as u32),
            );
        }
    }

    #[cfg(target_arch = "loongarch64")]
    {
        return IrqId::new(CPU_LOCAL_IRQ_DOMAIN, HwIrq(IPI_IRQ as u32));
    }

    IrqNumber(IPI_IRQ).expect("IPI IRQ exceeds legacy IRQ width")
}

/// IRQ handler.
///
/// # Warn
///
/// Make sure called in an interrupt context or hypervisor VM exit handler.
pub fn handle_irq(vector: usize) -> bool {
    let guard = ax_kernel_guard::NoPreempt::new();
    let handled = handle(TrapVector(vector)).is_some();

    drop(guard); // rescheduling may occur when preemption is re-enabled.
    handled
}

#[cfg(all(plat_dyn, target_arch = "x86_64"))]
pub fn set_ioapic_gsi_enabled_from_irq(gsi: u32, enabled: bool) -> Result<(), IrqError> {
    axplat_dyn::set_ioapic_gsi_enabled_from_irq(gsi, enabled)
}

/// Installs the default ArceOS IRQ dispatcher into `ax-cpu`'s runtime hook.
///
/// This is intended for runtimes that dispatch traps through
/// [`ax_cpu::trap::dispatch_irq`] instead of relying on the `#[irq_handler]`
/// link-time override path.
pub fn init_common_irq_handler() {
    let _ = set_irq_handler(handle_irq);
}
