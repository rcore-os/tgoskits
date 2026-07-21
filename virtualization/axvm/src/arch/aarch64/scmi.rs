//! VM-private SCMI transport for lease-filtered host clock and reset controls.

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
};

use arm_scmi_rs::{ScmiServer, ScmiServerBackend, ScmiServerOperationError, ScmiServerRequest};
use ax_kspin::SpinNoIrq as Mutex;
use axdevice::{DeviceBundle, DeviceRegistration};
use axdevice_base::{BusAccess, BusKind, BusResponse, Device, DeviceError, Resource};

use crate::{
    AxVmError, AxVmResult,
    machine::{
        ArmScmiMediationPlan, HostProviderControlError, HostProviderResourceControl,
        LeasedHostClock, LeasedHostReset,
    },
    vm::AxVMResources,
};

const CHANNEL_STATUS_OFFSET: usize = 0x04;
const CHANNEL_FREE: u32 = 1;
const SCMI_TRANSPORT_BUSY: usize = (-6_i64) as usize;
const SCMI_TRANSPORT_ERROR: usize = usize::MAX;

struct ManagedClock {
    control: LeasedHostClock,
    guest_enabled: AtomicBool,
}

/// One VM-local SCMI agent backed only by provider resources held by its lease.
pub(crate) struct Aarch64ScmiService {
    smc_function_id: u32,
    shared_memory: Arc<ScmiSharedMemoryDevice>,
    clocks: Vec<ManagedClock>,
    resets: Vec<LeasedHostReset>,
    in_flight: AtomicBool,
}

impl Aarch64ScmiService {
    pub(super) fn prepare(
        plan: &ArmScmiMediationPlan,
        resources: &AxVMResources,
    ) -> AxVmResult<(Arc<Self>, DeviceBundle)> {
        let mut clocks = Vec::with_capacity(plan.clocks().len());
        for claim in plan.clocks() {
            let control = match resources.host_provider_control(claim) {
                Some(HostProviderResourceControl::Clock(control)) => control,
                Some(other) => {
                    return Err(AxVmError::invalid_state(
                        "prepare AArch64 SCMI clock",
                        alloc::format!(
                            "claim for '{}' selector {:?} has incompatible control {other:?}",
                            claim.provider(),
                            claim.grant().reference().specifier(),
                        ),
                    ));
                }
                None => {
                    return Err(AxVmError::invalid_state(
                        "prepare AArch64 SCMI clock",
                        alloc::format!(
                            "claim for '{}' selector {:?} has no retained capability",
                            claim.provider(),
                            claim.grant().reference().specifier(),
                        ),
                    ));
                }
            };
            let guest_enabled = control
                .is_enabled()
                .map_err(|error| AxVmError::host("query assigned SCMI clock", error))?;
            clocks.push(ManagedClock {
                control,
                guest_enabled: AtomicBool::new(guest_enabled),
            });
        }

        let mut resets = Vec::with_capacity(plan.resets().len());
        for claim in plan.resets() {
            let control = match resources.host_provider_control(claim) {
                Some(HostProviderResourceControl::Reset(control)) => control,
                Some(other) => {
                    return Err(AxVmError::invalid_state(
                        "prepare AArch64 SCMI reset",
                        alloc::format!(
                            "claim for '{}' selector {:?} has incompatible control {other:?}",
                            claim.provider(),
                            claim.grant().reference().specifier(),
                        ),
                    ));
                }
                None => {
                    return Err(AxVmError::invalid_state(
                        "prepare AArch64 SCMI reset",
                        alloc::format!(
                            "claim for '{}' selector {:?} has no retained capability",
                            claim.provider(),
                            claim.grant().reference().specifier(),
                        ),
                    ));
                }
            };
            resets.push(control);
        }

        u32::try_from(clocks.len()).map_err(|_| {
            AxVmError::invalid_state("prepare AArch64 SCMI", "clock identifier space overflow")
        })?;
        u32::try_from(resets.len()).map_err(|_| {
            AxVmError::invalid_state("prepare AArch64 SCMI", "reset identifier space overflow")
        })?;

        let shared_memory = Arc::new(ScmiSharedMemoryDevice::new(plan.shared_memory())?);
        let service = Arc::new(Self {
            smc_function_id: plan.smc_function_id(),
            shared_memory: shared_memory.clone(),
            clocks,
            resets,
            in_flight: AtomicBool::new(false),
        });
        let bundle = DeviceBundle::from_registration(DeviceRegistration::Device(shared_memory));
        Ok((service, bundle))
    }

    /// Handles one SMC transport notification and returns its SMCCC result.
    pub(super) fn handle_smc(&self, function: u32) -> Option<usize> {
        if function != self.smc_function_id {
            return None;
        }
        let Ok(guard) = InFlightGuard::acquire(&self.in_flight) else {
            return Some(SCMI_TRANSPORT_BUSY);
        };
        let result = self.shared_memory.process(self);
        drop(guard);
        if let Err(error) = result {
            error!("failed to process VM-local SCMI transaction: {error}");
            return Some(SCMI_TRANSPORT_ERROR);
        }
        Some(0)
    }

    fn clock(&self, id: u32) -> Result<&ManagedClock, ScmiServerOperationError> {
        usize::try_from(id)
            .ok()
            .and_then(|id| self.clocks.get(id))
            .ok_or(ScmiServerOperationError::NotFound)
    }

    fn reset(&self, id: u32) -> Result<&LeasedHostReset, ScmiServerOperationError> {
        usize::try_from(id)
            .ok()
            .and_then(|id| self.resets.get(id))
            .ok_or(ScmiServerOperationError::NotFound)
    }
}

impl ScmiServerBackend for Aarch64ScmiService {
    fn clock_count(&self) -> u32 {
        self.clocks.len() as u32
    }

    fn clock_enabled(&self, id: u32) -> Result<bool, ScmiServerOperationError> {
        Ok(self.clock(id)?.guest_enabled.load(Ordering::Acquire))
    }

    fn clock_rate(&self, id: u32) -> Result<u64, ScmiServerOperationError> {
        self.clock(id)?.control.rate().map_err(map_control_error)
    }

    fn clock_set_rate(&self, id: u32, rate_hz: u64) -> Result<(), ScmiServerOperationError> {
        self.clock(id)?
            .control
            .set_rate(rate_hz)
            .map_err(map_control_error)
    }

    fn clock_configure(&self, id: u32, enabled: bool) -> Result<(), ScmiServerOperationError> {
        let clock = self.clock(id)?;
        if enabled {
            clock.control.enable().map_err(map_control_error)?;
        }
        // The host clock remains physically pinned while its device lease is
        // active. A guest disable changes only its private SCMI-visible state.
        clock.guest_enabled.store(enabled, Ordering::Release);
        Ok(())
    }

    fn reset_count(&self) -> u32 {
        self.resets.len() as u32
    }

    fn reset_asserted(&self, id: u32) -> Result<bool, ScmiServerOperationError> {
        self.reset(id)?.is_asserted().map_err(map_control_error)
    }

    fn reset_set(&self, id: u32, asserted: bool) -> Result<(), ScmiServerOperationError> {
        let reset = self.reset(id)?;
        if asserted {
            reset.assert()
        } else {
            reset.deassert()
        }
        .map_err(map_control_error)
    }
}

fn map_control_error(error: HostProviderControlError) -> ScmiServerOperationError {
    match error {
        HostProviderControlError::Unsupported { .. } => ScmiServerOperationError::NotSupported,
        HostProviderControlError::Backend { .. } => ScmiServerOperationError::Hardware,
    }
}

struct InFlightGuard<'a> {
    state: &'a AtomicBool,
}

impl<'a> InFlightGuard<'a> {
    fn acquire(state: &'a AtomicBool) -> Result<Self, ()> {
        state
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map(|_| Self { state })
            .map_err(|_| ())
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.state.store(false, Ordering::Release);
    }
}

struct ScmiSharedMemoryDevice {
    base: u64,
    size: usize,
    bytes: Mutex<Box<[u8]>>,
    resources: Box<[Resource]>,
}

impl ScmiSharedMemoryDevice {
    fn new(range: crate::machine::AddressRange) -> AxVmResult<Self> {
        let size = usize::try_from(range.size()).map_err(|_| {
            AxVmError::invalid_config("AArch64 SCMI shared-memory size exceeds usize")
        })?;
        let mut bytes = vec![0; size].into_boxed_slice();
        let status = bytes
            .get_mut(CHANNEL_STATUS_OFFSET..CHANNEL_STATUS_OFFSET + size_of::<u32>())
            .ok_or_else(|| {
                AxVmError::invalid_config("AArch64 SCMI shared-memory window is too short")
            })?;
        status.copy_from_slice(&CHANNEL_FREE.to_le_bytes());
        Ok(Self {
            base: range.base(),
            size,
            bytes: Mutex::new(bytes),
            resources: vec![Resource::MmioRange {
                base: range.base(),
                size: range.size(),
            }]
            .into_boxed_slice(),
        })
    }

    fn process(&self, backend: &dyn ScmiServerBackend) -> Result<(), DeviceError> {
        let request: ScmiServerRequest = {
            let mut bytes = self.bytes.lock();
            match ScmiServer::decode_request(&bytes) {
                Ok(request) => request,
                Err(error) => {
                    warn!("rejected malformed VM-local SCMI request: {error}");
                    ScmiServer::encode_protocol_error(&mut bytes).map_err(codec_error)?;
                    return Ok(());
                }
            }
        };

        // Provider callbacks can acquire platform locks. Keep them outside the
        // shared-memory critical section to preserve a single lock order.
        let response = ScmiServer::execute(&request, backend);
        ScmiServer::encode_response(&mut self.bytes.lock(), &response).map_err(codec_error)
    }

    fn access_range(&self, access: &BusAccess) -> Result<(usize, usize), DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::InvalidInput {
                operation: "access AArch64 SCMI shared memory",
                detail: "SCMI shared memory is available only on the MMIO bus".into(),
            });
        }
        let width = access.width.size();
        let relative = access
            .addr
            .checked_sub(self.base)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        let end = relative
            .checked_add(width)
            .filter(|end| *end <= self.size)
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        if relative % width != 0 {
            return Err(DeviceError::InvalidInput {
                operation: "access AArch64 SCMI shared memory",
                detail: alloc::format!(
                    "address {:#x} is not aligned to a {width}-byte access",
                    access.addr,
                ),
            });
        }
        Ok((relative, end))
    }
}

impl Device for ScmiSharedMemoryDevice {
    fn name(&self) -> &str {
        "arm-scmi-shmem"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let (offset, end) = self.access_range(access)?;
        let mut bytes = self.bytes.lock();
        if access.is_read {
            let mut value = [0; size_of::<u64>()];
            value[..access.width.size()].copy_from_slice(&bytes[offset..end]);
            Ok(BusResponse::Read {
                value: u64::from_le_bytes(value),
            })
        } else {
            bytes[offset..end].copy_from_slice(&access.data.to_le_bytes()[..access.width.size()]);
            Ok(BusResponse::Write)
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn codec_error(error: impl core::fmt::Display) -> DeviceError {
    DeviceError::InvalidData {
        operation: "process AArch64 SCMI shared memory",
        detail: alloc::format!("{error}"),
    }
}
