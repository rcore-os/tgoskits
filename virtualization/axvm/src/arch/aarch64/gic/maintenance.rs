//! Per-CPU host interrupt used to force VGIC state folding after guest EOI.

use axvm_types::{VmBackendError as BackendError, VmBackendResult as BackendResult};
use fdt_edit::Fdt;

pub(super) struct MaintenanceInterrupt {
    intid: u32,
    line: ax_std::os::arceos::modules::ax_hal::irq::IrqId,
}

pub(super) fn discover() -> BackendResult<MaintenanceInterrupt> {
    use ax_std::os::arceos::modules::ax_hal::irq;

    let intid = discover_host_maintenance_intid()?;
    let line = irq::resolve_percpu_irq(irq::HwIrq(intid)).map_err(map_irq_error)?;
    Ok(MaintenanceInterrupt { intid, line })
}

pub(super) fn enable_current_cpu() -> BackendResult {
    let host = super::host::get().ok_or(BackendError::InvalidState)?;
    set_enabled(&host.maintenance, true)
}

pub(super) fn disable_current_cpu() -> BackendResult {
    let host = super::host::get().ok_or(BackendError::InvalidState)?;
    set_enabled(&host.maintenance, false)
}

pub(super) fn matches_token(token: usize) -> bool {
    super::host::get().is_some_and(|host| host.maintenance.intid == super::host_irq_intid(token))
}

fn discover_host_maintenance_intid() -> BackendResult<u32> {
    let bytes = crate::boot::fdt::core::try_get_host_fdt().ok_or_else(|| {
        warn!("AArch64 VGIC requires a host FDT maintenance PPI");
        BackendError::Unsupported
    })?;
    let fdt = Fdt::from_bytes(bytes).map_err(|error| {
        warn!("cannot parse the host FDT while discovering the VGIC maintenance PPI: {error:?}");
        BackendError::InvalidData
    })?;
    super::super::fdt::host_gic_maintenance_intid(&fdt)
        .map_err(|error| {
            warn!("cannot decode the host VGIC maintenance PPI: {error:?}");
            BackendError::InvalidData
        })?
        .ok_or_else(|| {
            warn!("the host GIC does not describe a VGIC maintenance PPI");
            BackendError::Unsupported
        })
}

fn set_enabled(interrupt: &MaintenanceInterrupt, enabled: bool) -> BackendResult {
    use ax_std::os::arceos::modules::ax_hal::irq;

    irq::set_enable(interrupt.line, enabled).map_err(map_irq_error)
}

fn map_irq_error(error: ax_std::os::arceos::modules::ax_hal::irq::IrqError) -> BackendError {
    use ax_std::os::arceos::modules::ax_hal::irq::IrqError;

    match error {
        IrqError::InvalidIrq | IrqError::InvalidCpu => BackendError::InvalidInput,
        IrqError::NoMemory => BackendError::OutOfMemory,
        IrqError::Busy => BackendError::ResourceBusy,
        IrqError::Unsupported => BackendError::Unsupported,
        IrqError::CpuOffline
        | IrqError::Timeout
        | IrqError::NotFound
        | IrqError::InIrqContext
        | IrqError::Controller => BackendError::InvalidState,
    }
}
