//! Architecture-neutral 16550 model backed by the host console.

use alloc::sync::Arc;
use core::ops::ControlFlow;

use axdevice::{
    ConsoleRxPolicy, DeviceBackend, DeviceBuildContext, DeviceBundle, DeviceManagerError,
    DeviceManagerResult, DeviceModelId, DeviceRegistration, DeviceRequirements, DeviceTemplate,
    InterruptSharing, PollableDeviceOps, ResolvedDeviceResources, ResourceSlot, VirtualDeviceModel,
    VirtualDeviceModelRegistry,
};
use axvm_types::InterruptTriggerMode;
use virtual_ns16550::{Ns16550, Ns16550Backend, Ns16550BackendError, Ns16550RegisterLayout};

use crate::vm::host_console::{HostConsoleRxLease, HostConsoleTxLease};

const NS16550_MODEL_ID: &str = "ns16550a";
pub(crate) const DW_APB_UART_MODEL_ID: &str = "snps-dw-apb-uart";
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
            InterruptSharing::Exclusive,
        )
}

pub(crate) fn register_ns16550_model(
    registry: &mut VirtualDeviceModelRegistry,
    mmio_size: u64,
) -> DeviceManagerResult {
    register_model(registry, Ns16550ModelKind::Packed, mmio_size)
}

pub(crate) fn register_dw_apb_uart_model(
    registry: &mut VirtualDeviceModelRegistry,
    mmio_size: u64,
) -> DeviceManagerResult {
    register_model(registry, Ns16550ModelKind::DwApb, mmio_size)
}

fn register_model(
    registry: &mut VirtualDeviceModelRegistry,
    kind: Ns16550ModelKind,
    mmio_size: u64,
) -> DeviceManagerResult {
    registry.register(Arc::new(Ns16550Model::new(kind, mmio_size)?))
}

struct Ns16550Model {
    id: DeviceModelId,
    kind: Ns16550ModelKind,
    mmio_size: u64,
}

impl Ns16550Model {
    fn new(kind: Ns16550ModelKind, mmio_size: u64) -> DeviceManagerResult<Self> {
        ns16550_device_requirements(mmio_size)?;
        Ok(Self {
            id: DeviceModelId::new(kind.model_id())?,
            kind,
            mmio_size,
        })
    }
}

#[derive(Clone, Copy)]
enum Ns16550ModelKind {
    Packed,
    DwApb,
}

impl Ns16550ModelKind {
    const fn model_id(self) -> &'static str {
        match self {
            Self::Packed => NS16550_MODEL_ID,
            Self::DwApb => DW_APB_UART_MODEL_ID,
        }
    }

    const fn register_layout(self) -> Ns16550RegisterLayout {
        match self {
            Self::Packed => Ns16550RegisterLayout::Packed,
            Self::DwApb => Ns16550RegisterLayout::DwApb,
        }
    }

    fn matches_template(self, template: &DeviceTemplate) -> bool {
        match self {
            Self::Packed => {
                template.has_compatible("ns16550a") || template.has_compatible("ns16550")
            }
            Self::DwApb => template.has_compatible("snps,dw-apb-uart"),
        }
    }
}

impl VirtualDeviceModel for Ns16550Model {
    fn model_id(&self) -> DeviceModelId {
        self.id.clone()
    }

    fn matches_template(&self, template: &DeviceTemplate) -> bool {
        self.kind.matches_template(template)
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
        let irq = context.irq(&resource_slot(IRQ_SLOT)?)?;
        let (backend, rx_lease) = serial_backend(context.backend())?;
        let uart = Arc::new(
            Ns16550::new_mmio_with_layout(
                self.kind.model_id(),
                base,
                size,
                irq,
                backend,
                self.kind.register_layout(),
            )
            .map_err(|error| DeviceManagerError::InvalidConfig {
                operation: "build virtual 16550",
                detail: alloc::format!("{error}"),
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
        while self.uart.receive_ready() {
            let available = self.rx_lease.with_next_byte(|byte| {
                self.uart
                    .receive(byte)
                    .map(|_| ControlFlow::Continue(()))
                    .map_err(|error| DeviceManagerError::Backend {
                        operation: "poll virtual 16550 input",
                        detail: alloc::format!("{error}"),
                    })
            })?;
            if !available {
                break;
            }
        }
        Ok(())
    }
}
