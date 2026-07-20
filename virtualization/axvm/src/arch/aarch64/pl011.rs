//! AArch64 PL011 virtual-device model adapter.

use alloc::sync::Arc;

use arm_vpl011::{Pl011, Pl011Backend, Pl011BackendError};
use axdevice::{
    ConsoleRxPolicy, DeviceBackend, DeviceBuildContext, DeviceBundle, DeviceManagerError,
    DeviceManagerResult, DeviceModelId, DeviceRegistration, DeviceRequirements, DeviceTemplate,
    InterruptSharing, PollableDeviceOps, ResolvedDeviceResources, ResourceSlot, VirtualDeviceModel,
};
use axvm_types::InterruptTriggerMode;

use crate::vm::host_console::{HostConsoleRxLease, HostConsoleTxLease};

const MODEL_ID: &str = "arm-pl011";
const REGISTERS_SLOT: &str = "registers";
const IRQ_SLOT: &str = "irq";
const MMIO_SIZE: u64 = 0x1000;
const RECEIVE_TIMEOUT_NS: u64 = 3_000_000;

/// Returns the named resources required by every PL011 instance.
pub fn pl011_device_requirements() -> DeviceManagerResult<DeviceRequirements> {
    DeviceRequirements::new()
        .with_mmio(resource_slot(REGISTERS_SLOT)?, MMIO_SIZE, MMIO_SIZE)?
        .with_wired_irq(
            resource_slot(IRQ_SLOT)?,
            InterruptTriggerMode::LevelTriggered,
            InterruptSharing::Exclusive,
        )
}

/// Two-phase PL011 model whose backend is selected per device instance.
pub struct Aarch64Pl011Model {
    id: DeviceModelId,
}

impl Aarch64Pl011Model {
    /// Creates the architecture PL011 model.
    pub fn new() -> DeviceManagerResult<Self> {
        Ok(Self {
            id: DeviceModelId::new(MODEL_ID)?,
        })
    }
}

impl VirtualDeviceModel for Aarch64Pl011Model {
    fn model_id(&self) -> DeviceModelId {
        self.id.clone()
    }

    fn matches_template(&self, template: &DeviceTemplate) -> bool {
        template.has_compatible("arm,pl011") || template.has_compatible("arm,primecell")
    }

    fn requirements(
        &self,
        _template: Option<&DeviceTemplate>,
    ) -> DeviceManagerResult<DeviceRequirements> {
        pl011_device_requirements()
    }

    fn build(
        &self,
        resources: &ResolvedDeviceResources,
        context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let (base, size) = resources.mmio(&resource_slot(REGISTERS_SLOT)?)?;
        if size != MMIO_SIZE {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build PL011",
                detail: alloc::format!(
                    "register window has size {size:#x}, expected {MMIO_SIZE:#x}"
                ),
            });
        }
        let irq = context.irq(&resource_slot(IRQ_SLOT)?)?;
        let (backend, rx_lease) = serial_backend(context.backend())?;
        let device = Arc::new(Pl011::new("pl011", base, irq, backend).map_err(|error| {
            DeviceManagerError::InvalidConfig {
                operation: "build PL011",
                detail: alloc::format!("{error}"),
            }
        })?);
        let mut bundle =
            DeviceBundle::from_registration(DeviceRegistration::Device(device.clone()));
        if let Some(rx_lease) = rx_lease {
            bundle.push(DeviceRegistration::Pollable(Arc::new(Pl011ConsolePoller {
                device,
                rx_lease,
                input: ax_kspin::SpinNoIrq::new(BufferedConsoleInput::new()),
            })));
        }
        Ok(bundle)
    }
}

fn resource_slot(name: &'static str) -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new(name)
}

pub(super) fn register_standard_model(
    registry: &mut axdevice::VirtualDeviceModelRegistry,
) -> DeviceManagerResult {
    registry.register(Arc::new(Aarch64Pl011Model::new()?))
}

fn serial_backend(
    backend: DeviceBackend,
) -> DeviceManagerResult<(Arc<dyn Pl011Backend>, Option<HostConsoleRxLease>)> {
    match backend {
        DeviceBackend::None => Ok((Arc::new(Pl011OutputBackend { tx: None }), None)),
        DeviceBackend::HostConsole(policy) => {
            let tx = HostConsoleTxLease::claim(policy.tx())?;
            let rx = match policy.rx() {
                ConsoleRxPolicy::Exclusive => Some(HostConsoleRxLease::claim()?),
                ConsoleRxPolicy::Disabled => None,
            };
            Ok((Arc::new(Pl011OutputBackend { tx }), rx))
        }
    }
}

struct Pl011OutputBackend {
    tx: Option<HostConsoleTxLease>,
}

impl Pl011Backend for Pl011OutputBackend {
    fn transmit(&self, byte: u8) -> Result<(), Pl011BackendError> {
        if let Some(tx) = &self.tx {
            tx.write(&[byte]);
        }
        Ok(())
    }
}

struct Pl011ConsolePoller {
    device: Arc<Pl011>,
    rx_lease: HostConsoleRxLease,
    input: ax_kspin::SpinNoIrq<BufferedConsoleInput>,
}

struct BufferedConsoleInput {
    bytes: [u8; 32],
    next: usize,
    end: usize,
    last_receive_ns: Option<u64>,
}

impl BufferedConsoleInput {
    const fn new() -> Self {
        Self {
            bytes: [0; 32],
            next: 0,
            end: 0,
            last_receive_ns: None,
        }
    }

    const fn has_pending(&self) -> bool {
        self.next != self.end
    }

    fn refill(&mut self, rx_lease: &HostConsoleRxLease) {
        if self.has_pending() {
            return;
        }
        self.next = 0;
        self.end = rx_lease.read(&mut self.bytes).min(self.bytes.len());
    }
}

impl PollableDeviceOps for Pl011ConsolePoller {
    fn poll(&self, now_ns: u64) -> DeviceManagerResult {
        let mut input = self.input.lock();
        input.refill(&self.rx_lease);

        let mut accepted = false;
        while input.has_pending() && self.device.receive_ready() {
            let byte = input.bytes[input.next];
            match self
                .device
                .receive(byte, arm_vpl011::RxErrors::empty())
                .map_err(pl011_poll_error)?
            {
                arm_vpl011::RxResult::Accepted => {
                    input.next += 1;
                    accepted = true;
                }
                arm_vpl011::RxResult::DroppedOverrun | arm_vpl011::RxResult::ReceiverDisabled => {
                    break;
                }
            }
        }
        if accepted {
            input.last_receive_ns = Some(now_ns);
            return Ok(());
        }
        if input.has_pending() {
            return Ok(());
        }

        let should_expire = input
            .last_receive_ns
            .is_some_and(|last| now_ns.saturating_sub(last) >= RECEIVE_TIMEOUT_NS);
        if should_expire {
            input.last_receive_ns = None;
            drop(input);
            self.device
                .expire_receive_timeout()
                .map_err(pl011_poll_error)?;
        }
        Ok(())
    }
}

fn pl011_poll_error(error: arm_vpl011::Pl011Error) -> DeviceManagerError {
    DeviceManagerError::Backend {
        operation: "poll PL011 host-console input",
        detail: alloc::format!("{error}"),
    }
}
