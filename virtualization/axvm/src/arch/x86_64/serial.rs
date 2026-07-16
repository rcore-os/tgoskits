//! x86 COM1 model and AxVM runtime adapter.

use alloc::sync::Arc;

use axdevice::{
    ConsoleRxPolicy, DeviceBackend, DeviceBuildContext, DeviceBundle, DeviceManagerError,
    DeviceManagerResult, DeviceModelId, DeviceRegistration, DeviceRequirements,
    InterruptSourceKind, PollableDeviceOps, ResourceSlot, VirtualDeviceModel,
    VirtualDeviceModelRegistry, X86SerialBackend,
};
use axdevice_base::Device;
use axvm_types::InterruptTriggerMode;

use crate::vm::host_console::{HostConsoleRxLease, HostConsoleTxLease};

const COM1_BASE: u16 = 0x3f8;
const COM1_SIZE: u16 = 8;
const MODEL_NAME: &str = "x86-com1";
const REGISTERS_SLOT: &str = "registers";
const IRQ_SLOT: &str = "irq";

/// Returns the named resources required by the standard x86 COM1 model.
pub fn x86_com1_device_requirements() -> DeviceManagerResult<DeviceRequirements> {
    DeviceRequirements::new()
        .with_pio(ResourceSlot::new(REGISTERS_SLOT)?, COM1_SIZE, COM1_SIZE)?
        .with_wired_irq(
            ResourceSlot::new(IRQ_SLOT)?,
            InterruptTriggerMode::LevelTriggered,
            InterruptSourceKind::Software,
        )
}

pub(super) fn register_standard_model(
    registry: &mut VirtualDeviceModelRegistry,
) -> DeviceManagerResult {
    registry.register(Arc::new(X86Com1Model::new()?))
}

struct X86Com1Model {
    id: DeviceModelId,
}

impl X86Com1Model {
    fn new() -> DeviceManagerResult<Self> {
        Ok(Self {
            id: DeviceModelId::new(MODEL_NAME)?,
        })
    }
}

impl VirtualDeviceModel for X86Com1Model {
    fn model_id(&self) -> DeviceModelId {
        self.id.clone()
    }

    fn requirements(
        &self,
        _template: Option<&axdevice::DeviceTemplate>,
    ) -> DeviceManagerResult<DeviceRequirements> {
        x86_com1_device_requirements()
    }

    fn build(
        &self,
        resources: &axdevice::ResolvedDeviceResources,
        context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let registers = ResourceSlot::new(REGISTERS_SLOT)?;
        let (base, size) = resources.pio(&registers)?;
        if (base, size) != (COM1_BASE, COM1_SIZE) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 COM1",
                detail: alloc::format!(
                    "the current 16550 core requires ports {COM1_BASE:#x}..{:#x}, got \
                     {base:#x}..{:#x}",
                    u32::from(COM1_BASE) + u32::from(COM1_SIZE),
                    u32::from(base) + u32::from(size),
                ),
            });
        }

        let irq = context.irq(&ResourceSlot::new(IRQ_SLOT)?)?;
        let backend = serial_backend(context.backend())?;
        let serial = Arc::new(axdevice::X86SerialPortDevice::new_with_irq_and_backend(
            irq, backend,
        ));
        let poller = Arc::new(X86Com1Poller {
            serial: serial.clone(),
        });
        Ok(DeviceBundle::new()
            .with_registration(DeviceRegistration::Device(serial as Arc<dyn Device>))
            .with_registration(DeviceRegistration::Pollable(poller)))
    }
}

struct X86Com1Poller {
    serial: Arc<axdevice::X86SerialPortDevice>,
}

impl PollableDeviceOps for X86Com1Poller {
    fn poll(&self, _now_ns: u64) -> DeviceManagerResult {
        use axdevice::X86SerialDeviceOps as _;

        self.serial.service_irq().map(|_| ()).map_err(Into::into)
    }
}

fn serial_backend(backend: DeviceBackend) -> DeviceManagerResult<Arc<dyn X86SerialBackend>> {
    match backend {
        DeviceBackend::None => Ok(Arc::new(AxvmSerialBackend { rx: None, tx: None })),
        DeviceBackend::HostConsole(policy) => {
            let rx = match policy.rx() {
                ConsoleRxPolicy::Exclusive => Some(HostConsoleRxLease::claim()?),
                ConsoleRxPolicy::Disabled => None,
            };
            let tx = HostConsoleTxLease::claim(policy.tx())?;
            Ok(Arc::new(AxvmSerialBackend { rx, tx }))
        }
    }
}

struct AxvmSerialBackend {
    rx: Option<HostConsoleRxLease>,
    tx: Option<HostConsoleTxLease>,
}

impl X86SerialBackend for AxvmSerialBackend {
    fn transmit(&self, bytes: &[u8]) {
        if let Some(tx) = &self.tx {
            tx.write(bytes);
        }
    }

    fn receive(&self, bytes: &mut [u8]) -> usize {
        self.rx.as_ref().map_or(0, |rx| rx.read(bytes))
    }
}
