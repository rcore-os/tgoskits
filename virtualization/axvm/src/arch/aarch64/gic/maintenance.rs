//! Host GIC maintenance interrupt registration.

use ax_std::os::arceos::modules::ax_hal::irq::{self as host_irq, IrqHandle};

use super::Aarch64InterruptRoles;
use crate::{AxVmError, AxVmResult, vm::prepare::vcpus::VcpuPlacement};

/// RAII ownership of the per-CPU host maintenance action used by one VM.
pub(crate) struct HostMaintenanceInterrupt {
    registration: IrqHandle,
}

impl Drop for HostMaintenanceInterrupt {
    fn drop(&mut self) {
        if let Err(error) = host_irq::free_irq(self.registration) {
            warn!("failed to release the GICv3 maintenance IRQ action: {error:?}");
        }
    }
}

/// Registers the ICH underflow/maintenance PPI on every CPU that can run a
/// vCPU from this VM.
pub(crate) fn register(
    roles: &Aarch64InterruptRoles,
    placements: &[VcpuPlacement],
) -> AxVmResult<HostMaintenanceInterrupt> {
    let intid = u32::from(roles.maintenance_interrupt().raw());
    let irq = host_irq::resolve_percpu_irq(host_irq::HwIrq(intid)).map_err(|error| {
        AxVmError::interrupt(
            "resolve GICv3 maintenance interrupt",
            alloc::format!("INTID {intid}: {error:?}"),
        )
    })?;
    let cpus = target_cpu_mask(placements)?;
    let request = host_irq::IrqRequest::new_concurrent(|_| host_irq::IrqReturn::Handled)
        .scope(host_irq::IrqScope::PerCpu { cpus })
        .share_mode(host_irq::ShareMode::Shared);
    let registration = host_irq::request_irq(irq, request).map_err(|error| {
        AxVmError::interrupt(
            "register GICv3 maintenance interrupt",
            alloc::format!("host IRQ {irq:?}, CPUs {cpus:?}: {error:?}"),
        )
    })?;
    Ok(HostMaintenanceInterrupt { registration })
}

fn target_cpu_mask(placements: &[VcpuPlacement]) -> AxVmResult<host_irq::CpuMask> {
    let mut raw_mask = 0usize;
    for placement in placements {
        let Some(mask) = placement.phys_cpu_set else {
            return enabled_cpu_mask();
        };
        raw_mask |= mask;
    }
    cpu_mask_from_raw(raw_mask)
}

fn enabled_cpu_mask() -> AxVmResult<host_irq::CpuMask> {
    cpu_mask_from_raw(crate::percpu::enabled_cpu_mask())
}

fn cpu_mask_from_raw(raw: usize) -> AxVmResult<host_irq::CpuMask> {
    let mut cpus = host_irq::CpuMask::empty();
    for cpu in 0..usize::BITS as usize {
        if raw & (1usize << cpu) != 0 {
            cpus.insert(host_irq::CpuId(cpu));
        }
    }
    if cpus.is_empty() {
        return Err(AxVmError::invalid_config(
            "GICv3 maintenance interrupt has no target host CPU",
        ));
    }
    Ok(cpus)
}
