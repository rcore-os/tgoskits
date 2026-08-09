//! Process-wide host virtual-timer PPI registration.

use std::{format, sync::OnceLock, vec::Vec};

use ax_std::os::arceos::modules::ax_hal::irq;

use super::percpu::{PerCpuIrqControl, claim_enabled_percpu_irq};
use crate::{
    AxVmError, AxVmResult,
    host::{HostCpu, default_host},
};

/// The architectural CNTV PPI is a host capability, not a per-VM resource.
///
/// Like KVM's per-CPU arch-timer IRQ registration, AxVM claims it once for the
/// lifetime of the hypervisor. Whichever vCPU is loaded on a pCPU owns that
/// CPU's CNTV register state, so multiple VMs may safely time-share the same
/// physical line.
static HOST_TIMER_PPI: OnceLock<HostTimerPpiClaim> = OnceLock::new();

struct HostTimerPpiClaim {
    intid: u32,
    _handle: irq::IrqHandle,
}

pub(in crate::arch::aarch64) fn ensure_host_timer_ppi(intid: u32) -> AxVmResult {
    let claim = HOST_TIMER_PPI.get_or_try_init(|| HostTimerPpiClaim::install(intid))?;
    if claim.intid != intid {
        return Err(AxVmError::resource_conflict(
            "host virtual-timer PPI",
            format!(
                "INTID {} is already registered, but this machine profile requires INTID {intid}",
                claim.intid
            ),
        ));
    }
    Ok(())
}

impl HostTimerPpiClaim {
    fn install(intid: u32) -> AxVmResult<Self> {
        let irq = irq::resolve_percpu_irq(irq::HwIrq(intid))
            .map_err(|error| host_irq_error("resolve host virtual-timer PPI", error))?;
        let cpu_ids = (0..default_host().cpu_count()).collect::<Vec<_>>();
        let mut control = HostTimerPpiControl { irq };
        let handle = claim_enabled_percpu_irq(&mut control, intid, &cpu_ids)?;
        Ok(Self {
            intid,
            _handle: handle,
        })
    }
}

struct HostTimerPpiControl {
    irq: irq::IrqId,
}

impl PerCpuIrqControl for HostTimerPpiControl {
    type Claim = irq::IrqHandle;
    type Error = AxVmError;

    fn configure_level(&mut self, cpu_id: usize, hwirq: u32) -> Result<(), Self::Error> {
        if self.irq.hwirq != irq::HwIrq(hwirq) {
            return Err(AxVmError::invalid_state(
                "configure host virtual-timer PPI",
                format!(
                    "resolved hardware IRQ {} does not match requested INTID {hwirq}",
                    self.irq.hwirq.0
                ),
            ));
        }
        let mut request = ConfigureRequest {
            irq: self.irq,
            result: Ok(()),
        };
        crate::host::task::run_on_cpu_sync(
            cpu_id,
            configure_level_on_current_cpu,
            (&mut request as *mut ConfigureRequest).cast(),
        )
        .map_err(|error| {
            host_irq_error(
                "run host virtual-timer PPI configuration on target CPU",
                error,
            )
        })?;
        request.result.map_err(|error| {
            host_irq_error("configure host virtual-timer PPI as level-triggered", error)
        })
    }

    fn request_enabled(
        &mut self,
        hwirq: u32,
        cpu_ids: &[usize],
    ) -> Result<Self::Claim, Self::Error> {
        if self.irq.hwirq != irq::HwIrq(hwirq) {
            return Err(AxVmError::invalid_state(
                "claim host virtual-timer PPI",
                format!(
                    "resolved hardware IRQ {} does not match requested INTID {hwirq}",
                    self.irq.hwirq.0
                ),
            ));
        }
        let mut cpus = irq::CpuMask::empty();
        for &cpu_id in cpu_ids {
            let cpu = irq::CpuId(cpu_id);
            cpus.insert(cpu);
            if !cpus.contains(cpu) {
                return Err(AxVmError::unsupported(
                    "claim host virtual-timer PPI",
                    format!("CPU {cpu_id} cannot be represented by the host IRQ CPU mask"),
                ));
            }
        }
        irq::request_percpu_irq(self.irq, cpus, host_timer_ppi_fallback)
            .map_err(|error| host_irq_error("claim and enable host virtual-timer PPI", error))
    }
}

struct ConfigureRequest {
    irq: irq::IrqId,
    result: Result<(), irq::IrqError>,
}

/// # Safety
///
/// `arg` must point to a live [`ConfigureRequest`] until the synchronous
/// cross-CPU operation completes.
unsafe fn configure_level_on_current_cpu(arg: *mut ()) {
    let request = unsafe { &mut *arg.cast::<ConfigureRequest>() };
    request.result = irq::set_trigger(request.irq, irq::IrqTrigger::Level);
}

fn host_timer_ppi_fallback(_context: irq::IrqContext) -> irq::IrqReturn {
    // A CNTV PPI normally exits a running guest and is acknowledged by the
    // AxVM world-switch path. If it races with host context, the ordinary host
    // IRQ transaction performs priority-drop/deactivate after this fixed,
    // allocation-free handler returns.
    irq::IrqReturn::Handled
}

fn host_irq_error(operation: &'static str, error: irq::IrqError) -> AxVmError {
    AxVmError::interrupt(operation, format!("{error:?}"))
}
