//! Axvisor-owned capabilities attached to generic host-device claims.

use alloc::{boxed::Box, format, string::ToString};

use axvm::machine::{
    HostDeviceClaimProvider, HostDeviceId, HostDeviceLease, MachinePlanError, MachinePlanResult,
    RegisteredHostDeviceClaimProvider,
};

/// Claim provider that combines cross-VM exclusion with host capability leases.
pub(crate) struct AxvisorHostDeviceClaimProvider {
    registry: RegisteredHostDeviceClaimProvider,
    host_console: Option<HostDeviceId>,
}

impl AxvisorHostDeviceClaimProvider {
    /// Creates a provider for one planned snapshot and VM owner.
    pub(crate) const fn new(
        snapshot_generation: u64,
        vm_id: usize,
        host_console: Option<HostDeviceId>,
    ) -> Self {
        Self {
            registry: RegisteredHostDeviceClaimProvider::new(snapshot_generation, vm_id),
            host_console,
        }
    }
}

impl HostDeviceClaimProvider for AxvisorHostDeviceClaimProvider {
    fn snapshot_generation(&self) -> u64 {
        self.registry.snapshot_generation()
    }

    fn claim(&self, device: &HostDeviceId) -> MachinePlanResult<Box<dyn HostDeviceLease>> {
        let registry = self.registry.claim(device)?;
        let console = if self.host_console.as_ref() == Some(device) {
            Some(ax_hal::console::suspend_boot_output().map_err(|error| {
                MachinePlanError::ClaimRejected {
                    device: device.to_string(),
                    detail: format!("cannot transfer host console output: {error}"),
                }
            })?)
        } else {
            None
        };
        Ok(Box::new(AxvisorHostDeviceLease {
            _console: console,
            _registry: registry,
        }))
    }
}

struct AxvisorHostDeviceLease {
    _console: Option<ax_hal::console::BootConsoleOutputLease>,
    _registry: Box<dyn HostDeviceLease>,
}

impl HostDeviceLease for AxvisorHostDeviceLease {}
