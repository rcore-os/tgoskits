//! Axvisor-owned capabilities attached to generic host-device claims.

use alloc::{boxed::Box, format, string::ToString, sync::Arc};

#[cfg(target_arch = "aarch64")]
use axvm::machine::{
    HostClockControl, HostProviderControlError, HostProviderResourceControlKind,
    HostProviderResourceState, HostResetControl,
};
use axvm::machine::{
    HostDeviceClaimProvider, HostDeviceId, HostDeviceLease, HostProviderResourceClaim,
    HostProviderResourceLease, MachinePlanError, MachinePlanResult,
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

    fn claim_provider_resource(
        &self,
        resource: &HostProviderResourceClaim,
    ) -> MachinePlanResult<Arc<dyn HostProviderResourceLease>> {
        let registry = self.registry.claim_provider_resource(resource)?;
        #[cfg(target_arch = "aarch64")]
        match resource.grant().state() {
            HostProviderResourceState::MediatedClock => {
                return claim_clock(resource, registry);
            }
            HostProviderResourceState::MediatedReset => {
                return claim_reset(resource, registry);
            }
            HostProviderResourceState::FixedClock(_)
            | HostProviderResourceState::DeassertedReset => {}
        }
        Ok(registry)
    }
}

struct AxvisorHostDeviceLease {
    _console: Option<ax_hal::console::BootConsoleOutputLease>,
    _registry: Box<dyn HostDeviceLease>,
}

impl HostDeviceLease for AxvisorHostDeviceLease {}

#[cfg(target_arch = "aarch64")]
fn claim_clock(
    resource: &HostProviderResourceClaim,
    registry: Arc<dyn HostProviderResourceLease>,
) -> MachinePlanResult<Arc<dyn HostProviderResourceLease>> {
    let selector = single_selector(resource)? as usize;
    let device_id =
        rdrive::fdt_path_to_device_id(resource.provider().as_str()).ok_or_else(|| {
            rejected_resource(resource, "clock provider has no registered rdrive device")
        })?;
    let device = rdrive::get::<rdif_clk::Clk>(device_id)
        .map_err(|error| rejected_resource(resource, format!("clock lookup failed: {error}")))?;
    let clock_id = rdif_clk::ClockId::from(selector);
    let clock = device
        .lock()
        .map_err(|error| rejected_resource(resource, format!("clock lock failed: {error}")))?;
    let enabled = clock.is_enabled(clock_id).map_err(|error| {
        rejected_resource(resource, format!("clock state query failed: {error}"))
    })?;
    if !enabled {
        return Err(rejected_resource(
            resource,
            "clock is gated and the backend cannot restore a gated state",
        ));
    }
    let initial_rate = clock.get_rate(clock_id).map_err(|error| {
        rejected_resource(resource, format!("clock rate query failed: {error}"))
    })?;
    drop(clock);
    let control = AxvisorClockLease {
        device,
        clock_id,
        initial_rate,
        _registry: registry,
    };
    Ok(Arc::new(AxvisorClockResourceLease { control }))
}

#[cfg(target_arch = "aarch64")]
fn claim_reset(
    resource: &HostProviderResourceClaim,
    registry: Arc<dyn HostProviderResourceLease>,
) -> MachinePlanResult<Arc<dyn HostProviderResourceLease>> {
    let selector = single_selector(resource)?;
    let device_id =
        rdrive::fdt_path_to_device_id(resource.provider().as_str()).ok_or_else(|| {
            rejected_resource(resource, "reset provider has no registered rdrive device")
        })?;
    let device = rdrive::get::<rdif_reset::Reset>(device_id)
        .map_err(|error| rejected_resource(resource, format!("reset lookup failed: {error}")))?;
    let reset_id = rdif_reset::ResetId::from(selector);
    let reset = device
        .lock()
        .map_err(|error| rejected_resource(resource, format!("reset lock failed: {error}")))?;
    let initially_asserted = reset.is_asserted(reset_id).map_err(|error| {
        rejected_resource(resource, format!("reset state query failed: {error}"))
    })?;
    drop(reset);
    let control = AxvisorResetLease {
        device,
        reset_id,
        initially_asserted,
        _registry: registry,
    };
    Ok(Arc::new(AxvisorResetResourceLease { control }))
}

#[cfg(target_arch = "aarch64")]
fn single_selector(resource: &HostProviderResourceClaim) -> MachinePlanResult<u32> {
    let [selector] = resource.grant().reference().specifier() else {
        return Err(rejected_resource(
            resource,
            "AArch64 provider mediation requires one selector cell",
        ));
    };
    Ok(*selector)
}

#[cfg(target_arch = "aarch64")]
fn rejected_resource(
    resource: &HostProviderResourceClaim,
    detail: impl ToString,
) -> MachinePlanError {
    MachinePlanError::ClaimRejected {
        device: resource.provider().to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(target_arch = "aarch64")]
fn provider_error(operation: &'static str, error: impl ToString) -> HostProviderControlError {
    HostProviderControlError::Backend {
        operation,
        detail: error.to_string(),
    }
}

#[cfg(target_arch = "aarch64")]
struct AxvisorClockResourceLease {
    control: AxvisorClockLease,
}

#[cfg(target_arch = "aarch64")]
impl HostProviderResourceLease for AxvisorClockResourceLease {
    fn control_kind(&self) -> HostProviderResourceControlKind {
        HostProviderResourceControlKind::Clock
    }

    fn clock_control(&self) -> Option<&dyn HostClockControl> {
        Some(&self.control)
    }
}

#[cfg(target_arch = "aarch64")]
struct AxvisorClockLease {
    device: rdrive::Device<rdif_clk::Clk>,
    clock_id: rdif_clk::ClockId,
    initial_rate: u64,
    _registry: Arc<dyn HostProviderResourceLease>,
}

#[cfg(target_arch = "aarch64")]
impl HostClockControl for AxvisorClockLease {
    fn is_enabled(&self) -> Result<bool, HostProviderControlError> {
        self.device
            .lock()
            .map_err(|error| provider_error("query clock state", error))?
            .is_enabled(self.clock_id)
            .map_err(|error| provider_error("query clock state", error))
    }

    fn enable(&self) -> Result<(), HostProviderControlError> {
        self.device
            .lock()
            .map_err(|error| provider_error("enable clock", error))?
            .enable(self.clock_id)
            .map_err(|error| provider_error("enable clock", error))
    }

    fn rate(&self) -> Result<u64, HostProviderControlError> {
        self.device
            .lock()
            .map_err(|error| provider_error("query clock rate", error))?
            .get_rate(self.clock_id)
            .map_err(|error| provider_error("query clock rate", error))
    }

    fn set_rate(&self, rate_hz: u64) -> Result<(), HostProviderControlError> {
        self.device
            .lock()
            .map_err(|error| provider_error("set clock rate", error))?
            .set_rate(self.clock_id, rate_hz)
            .map_err(|error| provider_error("set clock rate", error))
    }
}

#[cfg(target_arch = "aarch64")]
impl Drop for AxvisorClockLease {
    fn drop(&mut self) {
        match self.device.lock() {
            Ok(mut clock) => {
                if let Err(error) = clock.set_rate(self.clock_id, self.initial_rate) {
                    log::error!(
                        "failed to restore host clock {:?} to {} Hz: {error}",
                        self.clock_id,
                        self.initial_rate,
                    );
                }
            }
            Err(error) => log::error!(
                "failed to lock host clock {:?} for restoration: {error}",
                self.clock_id,
            ),
        }
    }
}

#[cfg(target_arch = "aarch64")]
struct AxvisorResetResourceLease {
    control: AxvisorResetLease,
}

#[cfg(target_arch = "aarch64")]
impl HostProviderResourceLease for AxvisorResetResourceLease {
    fn control_kind(&self) -> HostProviderResourceControlKind {
        HostProviderResourceControlKind::Reset
    }

    fn reset_control(&self) -> Option<&dyn HostResetControl> {
        Some(&self.control)
    }
}

#[cfg(target_arch = "aarch64")]
struct AxvisorResetLease {
    device: rdrive::Device<rdif_reset::Reset>,
    reset_id: rdif_reset::ResetId,
    initially_asserted: bool,
    _registry: Arc<dyn HostProviderResourceLease>,
}

#[cfg(target_arch = "aarch64")]
impl HostResetControl for AxvisorResetLease {
    fn is_asserted(&self) -> Result<bool, HostProviderControlError> {
        self.device
            .lock()
            .map_err(|error| provider_error("query reset state", error))?
            .is_asserted(self.reset_id)
            .map_err(|error| provider_error("query reset state", error))
    }

    fn assert(&self) -> Result<(), HostProviderControlError> {
        self.device
            .lock()
            .map_err(|error| provider_error("assert reset", error))?
            .assert(self.reset_id)
            .map_err(|error| provider_error("assert reset", error))
    }

    fn deassert(&self) -> Result<(), HostProviderControlError> {
        self.device
            .lock()
            .map_err(|error| provider_error("deassert reset", error))?
            .deassert(self.reset_id)
            .map_err(|error| provider_error("deassert reset", error))
    }
}

#[cfg(target_arch = "aarch64")]
impl Drop for AxvisorResetLease {
    fn drop(&mut self) {
        match self.device.lock() {
            Ok(mut reset) => {
                let result = if self.initially_asserted {
                    reset.assert(self.reset_id)
                } else {
                    reset.deassert(self.reset_id)
                };
                if let Err(error) = result {
                    log::error!(
                        "failed to restore host reset {:?} to asserted={}: {error}",
                        self.reset_id,
                        self.initially_asserted,
                    );
                }
            }
            Err(error) => log::error!(
                "failed to lock host reset {:?} for restoration: {error}",
                self.reset_id,
            ),
        }
    }
}
