//! Per-CPU host interrupt used to force VGIC state folding after guest EOI.

use std::sync::OnceLock;

use axvm_types::{VmBackendError as BackendError, VmBackendResult as BackendResult};
use fdt_edit::Fdt;

static HOST_MAINTENANCE_INTID: OnceLock<u32> = OnceLock::new();

pub(super) fn enable_current_cpu() -> BackendResult {
    set_current_cpu_enabled(true)
}

pub(super) fn disable_current_cpu() -> BackendResult {
    let Some(intid) = HOST_MAINTENANCE_INTID.get().copied() else {
        return Ok(());
    };
    set_enabled(intid, false)
}

pub(super) fn matches_token(token: usize) -> bool {
    HOST_MAINTENANCE_INTID
        .get()
        .is_some_and(|intid| *intid == super::host_irq_intid(token))
}

fn set_current_cpu_enabled(enabled: bool) -> BackendResult {
    let intid = *HOST_MAINTENANCE_INTID.get_or_try_init(discover_host_maintenance_intid)?;
    set_enabled(intid, enabled)
}

fn discover_host_maintenance_intid() -> BackendResult<u32> {
    let bytes = super::super::fdt::try_get_host_fdt().ok_or_else(|| {
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

fn set_enabled(intid: u32, enabled: bool) -> BackendResult {
    use ax_std::os::arceos::modules::ax_hal::irq;

    let line = irq::resolve_percpu_irq(irq::HwIrq(intid)).map_err(map_irq_error)?;
    irq::set_enable(line, enabled).map_err(map_irq_error)
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
