//! Checked access to the host physical GICv3 implementation.

use arm_gic_driver::{
    checked_intid,
    v3::{Gic as PhysicalGicV3, Trigger as PhysicalTrigger},
};
use arm_vgic::{GicV3BackendError, GicV3HardwareCapabilities, TriggerMode, VgicError};
use ax_std::os::arceos::modules::ax_hal::irq::IrqId;

pub(super) fn physical_spi_count() -> Result<usize, GicV3BackendError> {
    with_physical_gic("inspect physical GIC capabilities", |gic| {
        capabilities(gic).map(GicV3HardwareCapabilities::spi_count)
    })
}

pub(super) fn with_physical_gic<R>(
    operation: &'static str,
    apply: impl FnOnce(&mut PhysicalGicV3) -> Result<R, GicV3BackendError>,
) -> Result<R, GicV3BackendError> {
    for intc in rdrive::get_list::<rdif_intc::Intc>() {
        let mut intc = intc.try_lock().map_err(|error| {
            GicV3BackendError::new(
                operation,
                alloc::format!("failed to lock a physical interrupt controller: {error:?}"),
            )
        })?;
        let Some(gic) = intc.typed_mut::<PhysicalGicV3>() else {
            continue;
        };
        return apply(gic);
    }
    Err(GicV3BackendError::new(
        operation,
        "no physical GICv3 backend is registered",
    ))
}

pub(super) fn checked_physical_spi(
    gic: &PhysicalGicV3,
    irq: IrqId,
    operation: &'static str,
) -> Result<arm_gic_driver::IntId, GicV3BackendError> {
    let raw = irq.hwirq.0;
    if raw < 32 {
        return Err(GicV3BackendError::new(
            operation,
            alloc::format!("host IRQ {irq:?} is private, not an assignable SPI"),
        ));
    }
    let interrupt_count = 32u32
        .checked_add(capabilities(gic)?.spi_count() as u32)
        .ok_or_else(|| {
            GicV3BackendError::new(operation, "physical GIC interrupt count overflowed")
        })?;
    checked_intid(raw, interrupt_count).map_err(|error| {
        GicV3BackendError::new(
            operation,
            alloc::format!("invalid physical SPI INTID {raw}: {error:?}"),
        )
    })
}

pub(super) fn physical_trigger(trigger: PhysicalTrigger) -> TriggerMode {
    match trigger {
        PhysicalTrigger::Edge => TriggerMode::Edge,
        PhysicalTrigger::Level => TriggerMode::Level,
    }
}

pub(super) fn physical_trigger_mode(trigger: TriggerMode) -> PhysicalTrigger {
    match trigger {
        TriggerMode::Edge => PhysicalTrigger::Edge,
        TriggerMode::Level => PhysicalTrigger::Level,
    }
}

pub(super) fn vgic_state_error(error: VgicError) -> GicV3BackendError {
    GicV3BackendError::new("access physical interrupt state", alloc::format!("{error}"))
}

pub(super) fn instruction_sync_barrier() {
    // SAFETY: `isb` only synchronizes preceding GIC register operations on the
    // current CPU and neither dereferences memory nor changes Rust-visible state.
    unsafe { core::arch::asm!("isb", options(nostack, preserves_flags)) };
}

fn capabilities(gic: &PhysicalGicV3) -> Result<GicV3HardwareCapabilities, GicV3BackendError> {
    GicV3HardwareCapabilities::from_distributor_typer(gic.typer_raw()).map_err(vgic_state_error)
}
