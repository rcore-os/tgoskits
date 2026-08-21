//! Runtime local-APIC glue for somehal.
//!
//! Register operations live in `x86-apic-driver`; this file only locates the
//! APIC page through the boot mapping and maps driver errors onto
//! `irq_framework` errors. The handle is plain configuration, so it is built
//! per call — the same one-MSR-read cost the previous in-file implementation
//! paid for per-call mode discovery.

use x86_apic_driver::{
    ApicError, LocalApicConfig, TimerDivide, TimerMode, VirtAddr, X86LocalApic,
    local_apic::{apic_phys_base, cpu_has_tsc_deadline},
};

use super::vector::{
    APIC_ERROR_VECTOR, APIC_IPI_VECTOR, APIC_TIMER_VECTOR, SPURIOUS_VECTOR, lapic_ipi_irq_id,
};
use crate::irq::{IrqError, IrqId};

pub(super) fn eoi() {
    local_apic().eoi();
}

pub(super) fn ipi_vector(irq: IrqId) -> Result<u8, IrqError> {
    if irq == lapic_ipi_irq_id() {
        Ok(APIC_IPI_VECTOR as u8)
    } else {
        Err(IrqError::InvalidIrq)
    }
}

pub(super) fn send_ipi_to_apic_id(apic_id: u32, vector: u8) -> Result<(), IrqError> {
    local_apic()
        .send_fixed_ipi(apic_id, vector)
        .map_err(map_apic_error)
}

pub(super) fn send_ipi(vector: u8) -> Result<(), IrqError> {
    local_apic().send_self_ipi(vector).map_err(map_apic_error)
}

/// Builds the local-APIC driver handle for the current CPU.
pub(super) fn local_apic() -> X86LocalApic {
    let mmio_base = VirtAddr::new(someboot::mem::phys_to_virt(apic_phys_base()) as usize);
    // SAFETY: `apic_phys_base` reads the LAPIC page from IA32_APIC_BASE, and
    // someboot's permanent direct mapping keeps the complete page valid for
    // the kernel lifetime. The driver dereferences it only in xAPIC mode.
    unsafe { X86LocalApic::new(lapic_config(), mmio_base) }
}

fn lapic_config() -> LocalApicConfig {
    LocalApicConfig {
        timer_vector: APIC_TIMER_VECTOR as u8,
        error_vector: APIC_ERROR_VECTOR as u8,
        spurious_vector: SPURIOUS_VECTOR as u8,
        timer_mode: if cpu_has_tsc_deadline() {
            TimerMode::TscDeadline
        } else {
            TimerMode::OneShot
        },
        timer_divide: TimerDivide::Div16,
        timer_initial: 0,
    }
}

fn map_apic_error(error: ApicError) -> IrqError {
    match error {
        ApicError::XapicDestinationOverflow(_) => IrqError::InvalidCpu,
        ApicError::IpiDeliveryTimeout => IrqError::Timeout,
        ApicError::LocalInterruptPinsUnmasked { .. } => IrqError::Controller,
        ApicError::ApicUnsupported(_) => IrqError::Unsupported,
        ApicError::InvalidIoApicInput(_) => IrqError::InvalidIrq,
    }
}
