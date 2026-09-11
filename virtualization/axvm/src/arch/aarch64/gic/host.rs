//! Host-wide GIC discovery, committed before any CPU enables virtualization.

use std::sync::OnceLock;

use super::{cpu_interface::HostCpuInterface, maintenance::MaintenanceInterrupt};
use crate::{AxVmError, AxVmResult};

pub(super) struct HostGic {
    pub(super) cpu_interface: HostCpuInterface,
    pub(super) maintenance: MaintenanceInterrupt,
}

static HOST_GIC: OnceLock<HostGic> = OnceLock::new();

/// Runs only on the sleepable host setup path, before per-CPU IRQ masking.
/// Failed discovery publishes nothing and enables no hardware, so it is retryable.
pub(crate) fn prepare() -> AxVmResult {
    HOST_GIC.get_or_try_init(|| {
        let cpu_interface = super::cpu_interface::discover()
            .map_err(|error| AxVmError::interrupt("discover host GIC CPU interface", error))?;
        let maintenance = super::maintenance::discover().map_err(|error| {
            AxVmError::interrupt("discover host GIC maintenance interrupt", error)
        })?;
        Ok::<_, AxVmError>(HostGic {
            cpu_interface,
            maintenance,
        })
    })?;
    Ok(())
}

/// IRQ and per-CPU paths may observe publication but must never wait for it.
pub(super) fn get() -> Option<&'static HostGic> {
    HOST_GIC.get()
}
