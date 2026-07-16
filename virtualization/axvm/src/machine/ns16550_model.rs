//! Architecture-neutral 16550 model backed by the host console.

use alloc::sync::Arc;

use axdevice::{
    ConsoleRxPolicy, DeviceBackend, DeviceBuildContext, DeviceBundle, DeviceManagerError,
    DeviceManagerResult, DeviceModelId, DeviceRegistration, DeviceRequirements, DeviceTemplate,
    InterruptSourceKind, PollableDeviceOps, ResolvedDeviceResources, ResourceSlot,
    VirtualDeviceModel, VirtualDeviceModelRegistry,
};
use axvm_types::InterruptTriggerMode;
use virtual_ns16550::{Ns16550, Ns16550Backend, Ns16550BackendError};

use crate::vm::host_console::{HostConsoleRxLease, HostConsoleTxLease};

const MODEL_ID: &str = "ns16550a";
const REGISTERS_SLOT: &str = "registers";
const IRQ_SLOT: &str = "irq";

pub(crate) fn ns16550_device_requirements(
    mmio_size: u64,
) -> DeviceManagerResult<DeviceRequirements> {
    DeviceRequirements::new()
        .with_mmio(resource_slot(REGISTERS_SLOT)?, mmio_size, mmio_size)?
        .with_wired_irq(
            resource_slot(IRQ_SLOT)?,
            InterruptTriggerMode::LevelTriggered,
            InterruptSourceKind::Software,
        )
}

pub(crate) fn register_ns16550_model(
    registry: &mut VirtualDeviceModelRegistry,
    mmio_size: u64,
) -> DeviceManagerResult {
    registry.register(Arc::new(Ns16550Model::new(mmio_size)?))
}

struct Ns16550Model {
    id: DeviceModelId,
    mmio_size: u64,
}

impl Ns16550Model {
    fn new(mmio_size: u64) -> DeviceManagerResult<Self> {
        ns16550_device_requirements(mmio_size)?;
        Ok(Self {
            id: DeviceModelId::new(MODEL_ID)?,
            mmio_size,
        })
    }
}

impl VirtualDeviceModel for Ns16550Model {
    fn model_id(&self) -> DeviceModelId {
        self.id.clone()
    }

    fn matches_template(&self, template: &DeviceTemplate) -> bool {
        template.has_compatible("ns16550a") || template.has_compatible("ns16550")
    }

    fn requirements(
        &self,
        _template: Option<&DeviceTemplate>,
    ) -> DeviceManagerResult<DeviceRequirements> {
        ns16550_device_requirements(self.mmio_size)
    }

    fn build(
        &self,
        resources: &ResolvedDeviceResources,
        context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let (base, size) = resources.mmio(&resource_slot(REGISTERS_SLOT)?)?;
        if size < 8 {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build virtual 16550",
                detail: alloc::format!("register window has size {size:#x}, expected at least 8"),
            });
        }
        let irq = context.irq(&resource_slot(IRQ_SLOT)?)?;
        let (backend, rx_lease) = serial_backend(context.backend())?;
        let uart = Arc::new(
            Ns16550::new_mmio("ns16550a", base, size, irq, backend).map_err(|error| {
                DeviceManagerError::InvalidConfig {
                    operation: "build virtual 16550",
                    detail: alloc::format!("{error}"),
                }
            })?,
        );
        let mut bundle =
            DeviceBundle::new().with_registration(DeviceRegistration::Device(uart.clone()));
        if let Some(rx_lease) = rx_lease {
            bundle.push(DeviceRegistration::Pollable(Arc::new(Ns16550Poller {
                uart,
                rx_lease,
            })));
        }
        Ok(bundle)
    }
}

fn resource_slot(name: &'static str) -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new(name)
}

fn serial_backend(
    backend: DeviceBackend,
) -> DeviceManagerResult<(Arc<dyn Ns16550Backend>, Option<HostConsoleRxLease>)> {
    match backend {
        DeviceBackend::None => Ok((Arc::new(Ns16550OutputBackend { tx: None }), None)),
        DeviceBackend::HostConsole(policy) => {
            let tx = HostConsoleTxLease::claim(policy.tx())?;
            let rx = match policy.rx() {
                ConsoleRxPolicy::Exclusive => Some(HostConsoleRxLease::claim()?),
                ConsoleRxPolicy::Disabled => None,
            };
            Ok((Arc::new(Ns16550OutputBackend { tx }), rx))
        }
    }
}

struct Ns16550OutputBackend {
    tx: Option<HostConsoleTxLease>,
}

impl Ns16550Backend for Ns16550OutputBackend {
    fn transmit(&self, byte: u8) -> Result<(), Ns16550BackendError> {
        if let Some(tx) = &self.tx {
            tx.write(&[byte]);
        }
        Ok(())
    }
}

struct Ns16550Poller {
    uart: Arc<Ns16550>,
    rx_lease: HostConsoleRxLease,
}

impl PollableDeviceOps for Ns16550Poller {
    fn poll(&self, _now_ns: u64) -> DeviceManagerResult {
        let mut bytes = [0; 32];
        let count = self.rx_lease.read(&mut bytes).min(bytes.len());
        for byte in &bytes[..count] {
            self.uart
                .receive(*byte)
                .map_err(|error| DeviceManagerError::Backend {
                    operation: "poll virtual 16550 input",
                    detail: alloc::format!("{error}"),
                })?;
        }
        Ok(())
    }
}
