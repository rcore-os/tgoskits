//! Runtime local-APIC glue for somehal.
//!
//! Register operations live in `x86-apic-driver`; this file owns one
//! initialized handle and timer profile in each runtime CPU area. Capability
//! discovery and APIC-page translation happen during that CPU's early init,
//! matching Linux's per-CPU LAPIC clockevent ownership.

use x86_apic_driver::{
    ApicError, LocalApicConfig, TimerDivide, TimerMode, VirtAddr, X86LocalApic,
    local_apic::apic_phys_base,
};

use super::vector::{
    APIC_ERROR_VECTOR, APIC_IPI_VECTOR, APIC_TIMER_VECTOR, SPURIOUS_VECTOR, lapic_ipi_irq_id,
};
use crate::irq::{IrqError, IrqId};

#[ax_percpu::def_percpu]
static LOCAL_APIC: CpuLocalApic = CpuLocalApic::offline();

#[derive(Clone, Copy)]
pub(super) struct LocalTimerProfile {
    pub(super) tsc_deadline: bool,
    pub(super) apic_counts_per_tsc_q32: u64,
}

struct CpuLocalApic {
    device: Option<X86LocalApic>,
    timer: LocalTimerProfile,
}

impl CpuLocalApic {
    const fn offline() -> Self {
        Self {
            device: None,
            timer: LocalTimerProfile {
                tsc_deadline: false,
                apic_counts_per_tsc_q32: 0,
            },
        }
    }
}

pub(super) fn eoi() {
    with_current_lapic(|lapic, _timer| lapic.eoi());
}

pub(super) fn ipi_vector(irq: IrqId) -> Result<u8, IrqError> {
    if irq == lapic_ipi_irq_id() {
        Ok(APIC_IPI_VECTOR as u8)
    } else {
        Err(IrqError::InvalidIrq)
    }
}

pub(super) fn send_ipi_to_apic_id(apic_id: u32, vector: u8) -> Result<(), IrqError> {
    with_current_lapic(|lapic, _timer| {
        lapic
            .send_fixed_ipi(apic_id, vector)
            .map_err(map_apic_error)
    })
}

pub(super) fn send_ipi(vector: u8) -> Result<(), IrqError> {
    with_current_lapic(|lapic, _timer| lapic.send_self_ipi(vector).map_err(map_apic_error))
}

/// Builds an offline local-APIC handle for the current CPU's early init.
pub(super) fn new_current_lapic(tsc_deadline: bool) -> X86LocalApic {
    let mmio_base = VirtAddr::new(someboot::mem::phys_to_virt(apic_phys_base()) as usize);
    // SAFETY: `apic_phys_base` reads the LAPIC page from IA32_APIC_BASE, and
    // someboot's permanent direct mapping keeps the complete page valid for
    // the kernel lifetime. The driver dereferences it only in xAPIC mode.
    unsafe { X86LocalApic::new(lapic_config(tsc_deadline), mmio_base) }
}

pub(super) fn install_current_lapic(device: X86LocalApic, timer: LocalTimerProfile) {
    // SAFETY: early per-CPU init runs with scheduling and local interrupts
    // offline. No runtime accessor can observe this slot until init returns.
    unsafe {
        LOCAL_APIC.with_current_cpu_area_mut(|slot| {
            assert!(
                slot.device.is_none(),
                "local APIC may only be installed once per CPU"
            );
            slot.device = Some(device);
            slot.timer = timer;
        })
    }
    .unwrap_or_else(|error| panic!("local APIC CPU area is unavailable during init: {error}"));
}

pub(super) fn with_current_lapic<R>(
    operation: impl FnOnce(&X86LocalApic, LocalTimerProfile) -> R,
) -> R {
    // SAFETY: callers are early boot, IRQ handling, or clockevent/IPI paths
    // that already exclude migration and context switches. The installed
    // device is immutable after early init and targets only the current CPU.
    unsafe {
        LOCAL_APIC.with_current_cpu_area(|slot| {
            let device = slot
                .device
                .as_ref()
                .expect("local APIC must be installed before runtime access");
            operation(device, slot.timer)
        })
    }
    .unwrap_or_else(|error| panic!("local APIC CPU area is unavailable at runtime: {error}"))
}

fn lapic_config(tsc_deadline: bool) -> LocalApicConfig {
    LocalApicConfig {
        timer_vector: APIC_TIMER_VECTOR as u8,
        error_vector: APIC_ERROR_VECTOR as u8,
        spurious_vector: SPURIOUS_VECTOR as u8,
        timer_mode: if tsc_deadline {
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
