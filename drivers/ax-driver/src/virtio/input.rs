//! VirtIO input split into owner-only queue state and destructive IRQ capture.

extern crate alloc;

use alloc::{
    borrow::ToOwned,
    boxed::Box,
    format,
    string::{String, ToString},
    sync::Arc,
};
use core::sync::atomic::{AtomicBool, Ordering};

use rdif_input::{
    AbsInfo, EventType, InputDeviceId, InputError, InputEvent, InputExecution, InputIrqEndpoint,
    InputIrqFault, IrqEvent,
};
use rdif_irq::{ContainmentCause, FaultContainment, IrqCapture, IrqEndpoint, MaskedSource};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(feature = "pci")]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::input::{InputConfigSelect, VirtIOInput},
    transport::InterruptStatus,
};

use crate::{
    BindingInfo, IrqBindingLease,
    input::PlatformDeviceInput,
    virtio::{VirtIoHalImpl, VirtIoTransport},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci_endpoint};

const MMIO_INTERRUPT_STATUS_OFFSET: usize = 0x60;
const MMIO_INTERRUPT_ACK_OFFSET: usize = 0x64;
const MMIO_INTERRUPT_REGISTERS_END: usize = MMIO_INTERRUPT_ACK_OFFSET + size_of::<u32>();

#[cfg(feature = "pci")]
const PCI_VIRTIO_ISR_CONFIG_TYPE: u8 = 3;
#[cfg(feature = "pci")]
const PCI_VIRTIO_ISR_CAP_MIN_LENGTH: u8 = 16;
#[cfg(feature = "pci")]
const PCI_CAP_BAR_OFFSET: u16 = 4;
#[cfg(feature = "pci")]
const PCI_CAP_REGION_OFFSET: u16 = 8;
#[cfg(feature = "pci")]
const PCI_CAP_REGION_LENGTH: u16 = 12;

#[cfg(feature = "pci")]
crate::model_register!(
    name: "VirtIO Input",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(feature = "pci")]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    crate::pci::ensure_virtio_pci_endpoint(probe.endpoint(), DeviceType::Input)?;
    let info = binding_info_from_pci_endpoint(
        probe.info(),
        probe.endpoint(),
        PciIrqRequirement::Required,
    )?;
    let interrupt_port = pci_interrupt_port(probe.endpoint())?;
    let (transport, irq_lease) = crate::pci::take_virtio_input_transport(
        probe.endpoint_mut(),
        DeviceType::Input,
        info.clone(),
    )?;
    register_transport_with_parts(
        probe.into_platform_device(),
        transport,
        interrupt_port,
        Some(irq_lease),
        info,
    )
}

/// Rejects a transport whose destructive interrupt-status capability vanished.
pub fn register_transport<T: VirtIoTransport>(
    _plat_dev: PlatformDevice,
    _transport: T,
) -> Result<(), OnProbeError> {
    Err(OnProbeError::other(
        "virtio input registration requires an independent interrupt port",
    ))
}

/// Registers a transport with a separately owned interrupt-status port.
pub fn register_transport_with_interrupt_port<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    interrupt_port: VirtioInputInterruptPort,
) -> Result<(), OnProbeError> {
    register_transport_with_parts(
        plat_dev,
        transport,
        interrupt_port,
        None,
        BindingInfo::empty(),
    )
}

/// Registers a statically discovered VirtIO MMIO input source.
pub fn register_mmio_transport<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    mapping: mmio_api::Mmio,
) -> Result<(), OnProbeError> {
    let interrupt_port = VirtioInputInterruptPort::from_mmio(mapping)
        .map_err(|error| OnProbeError::other(error.to_string()))?;
    register_transport_with_interrupt_port(plat_dev, transport, interrupt_port)
}

fn register_transport_with_parts<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    interrupt_port: VirtioInputInterruptPort,
    irq_lease: Option<crate::pci::PciIntxIrqLease>,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let dev = VirtIoInputDevice::discovered(transport, interrupt_port, irq_lease);
    let irq = plat_dev.register_input_with_info(dev, info);
    log::info!("discovered virtio input device irq={irq:?}");
    Ok(())
}

/// Destructive VirtIO interrupt-status capability owned by one IRQ action.
pub struct VirtioInputInterruptPort {
    registers: InputInterruptRegisters,
}

impl VirtioInputInterruptPort {
    /// Creates an MMIO endpoint while retaining the complete mapping lease.
    pub fn from_mmio(mapping: mmio_api::Mmio) -> Result<Self, InputError> {
        if mapping.size() < MMIO_INTERRUPT_REGISTERS_END {
            return Err(InputError::NotAvailable);
        }
        Ok(Self {
            registers: InputInterruptRegisters::Mmio(mapping),
        })
    }

    /// Creates a PCI endpoint from the transport ISR capability.
    pub fn from_pci_isr(mapping: mmio_api::Mmio) -> Result<Self, InputError> {
        if mapping.size() < size_of::<u8>() {
            return Err(InputError::NotAvailable);
        }
        Ok(Self {
            registers: InputInterruptRegisters::Pci(mapping),
        })
    }

    fn capture_status(&mut self) -> u32 {
        match &self.registers {
            InputInterruptRegisters::Mmio(mapping) => {
                let status = mapping.read::<u32>(MMIO_INTERRUPT_STATUS_OFFSET);
                if status != 0 {
                    mapping.write(MMIO_INTERRUPT_ACK_OFFSET, status);
                }
                status
            }
            // A VirtIO PCI ISR read is itself the acknowledgement.
            InputInterruptRegisters::Pci(mapping) => u32::from(mapping.read::<u8>(0)),
        }
    }
}

enum InputInterruptRegisters {
    Mmio(mmio_api::Mmio),
    Pci(mmio_api::Mmio),
}

struct VirtIoInputIrqEndpoint {
    port: VirtioInputInterruptPort,
    enabled: Arc<AtomicBool>,
    source_mask: Option<crate::pci::PciIntxSourceMask>,
}

impl IrqEndpoint for VirtIoInputIrqEndpoint {
    type Event = IrqEvent;
    type Fault = InputIrqFault;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        if !self.enabled.load(Ordering::Acquire) {
            return IrqCapture::Unhandled;
        }
        let raw = self.port.capture_status();
        if raw == 0 {
            return IrqCapture::Unhandled;
        }
        let status = InterruptStatus::from_bits_truncate(raw);
        if status.bits() != raw {
            let containment = match self.contain(ContainmentCause::CaptureFault) {
                Ok(source) => FaultContainment::DeviceSourceMasked(source),
                Err(_) => FaultContainment::Uncontained,
            };
            return IrqCapture::Fault {
                reason: InputIrqFault::InvalidStatus,
                containment,
            };
        }
        IrqCapture::Captured {
            event: input_irq_event(status),
            masked: None,
        }
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        let source_mask = self
            .source_mask
            .as_mut()
            .ok_or(InputIrqFault::Uncontained)?;
        self.enabled.store(false, Ordering::Release);
        Ok(source_mask.mask_from_irq())
    }
}

struct VirtIoInputDevice<T: VirtIoTransport> {
    transport: Option<T>,
    raw: Option<VirtIOInput<VirtIoHalImpl, T>>,
    device_id: InputDeviceId,
    name: String,
    physical_location: String,
    unique_id: String,
    irq_port: Option<VirtioInputInterruptPort>,
    irq_enabled: Arc<AtomicBool>,
    irq_lease: Option<crate::pci::PciIntxIrqLease>,
}

// SAFETY: all transport and queue mutation moves to one CPU-pinned maintenance
// owner before initialization. The detached IRQ endpoint shares only atomics
// and its independent ISR/source-mask capabilities.
unsafe impl<T: VirtIoTransport> Send for VirtIoInputDevice<T> {}

impl<T: VirtIoTransport> VirtIoInputDevice<T> {
    fn discovered(
        transport: T,
        irq_port: VirtioInputInterruptPort,
        irq_lease: Option<crate::pci::PciIntxIrqLease>,
    ) -> Self {
        Self {
            transport: Some(transport),
            raw: None,
            device_id: InputDeviceId {
                bus_type: 0,
                vendor: 0,
                product: 0,
                version: 0,
            },
            name: "virtio-input".to_owned(),
            physical_location: String::new(),
            unique_id: String::new(),
            irq_port: Some(irq_port),
            irq_enabled: Arc::new(AtomicBool::new(false)),
            irq_lease,
        }
    }

    fn raw_mut(&mut self) -> Result<&mut VirtIOInput<VirtIoHalImpl, T>, InputError> {
        self.raw.as_mut().ok_or(InputError::NotAvailable)
    }
}

impl<T: VirtIoTransport> DriverGeneric for VirtIoInputDevice<T> {
    fn name(&self) -> &str {
        &self.name
    }
}

impl<T: VirtIoTransport> rdif_input::Interface for VirtIoInputDevice<T> {
    fn initialize(&mut self) -> Result<(), InputError> {
        if self.raw.is_some() {
            return Ok(());
        }
        let transport = self.transport.take().ok_or(InputError::NotAvailable)?;
        let mut raw = VirtIOInput::new(transport).map_err(map_input_err)?;
        let name = raw.name().unwrap_or_else(|_| "virtio-input".to_owned());
        let id = raw.ids().map_err(map_input_err)?;
        self.device_id = InputDeviceId {
            bus_type: id.bustype,
            vendor: id.vendor,
            product: id.product,
            version: id.version,
        };
        self.physical_location = format!(
            "virtio-input/{:04x}:{:04x}:{:04x}:{:04x}",
            self.device_id.bus_type,
            self.device_id.vendor,
            self.device_id.product,
            self.device_id.version
        );
        self.unique_id = format!(
            "virtio-{:04x}-{:04x}-{:04x}-{:04x}-{name}",
            self.device_id.bus_type,
            self.device_id.vendor,
            self.device_id.product,
            self.device_id.version
        );
        self.name = name;
        self.raw = Some(raw);
        Ok(())
    }

    fn execution(&self) -> InputExecution {
        InputExecution::Interrupt
    }

    fn device_id(&self) -> InputDeviceId {
        self.device_id
    }

    fn physical_location(&self) -> &str {
        &self.physical_location
    }

    fn unique_id(&self) -> &str {
        &self.unique_id
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> Result<bool, InputError> {
        self.raw_mut()?
            .query_config_select(InputConfigSelect::EvBits, ty as u8, out)
            .map(|read| read != 0)
            .map_err(map_input_err)
    }

    fn read_event(&mut self) -> Result<InputEvent, InputError> {
        let event = self
            .raw_mut()?
            .pop_pending_event()
            .ok_or(InputError::Again)?;
        Ok(InputEvent {
            event_type: event.event_type,
            code: event.code,
            value: event.value as i32,
        })
    }

    fn get_prop_bits(&mut self, out: &mut [u8]) -> Result<usize, InputError> {
        let bits = self.raw_mut()?.prop_bits().map_err(map_input_err)?;
        let len = bits.len().min(out.len());
        out[..len].copy_from_slice(&bits[..len]);
        Ok(len)
    }

    fn get_abs_info(&mut self, axis: u8) -> Result<AbsInfo, InputError> {
        let info = self.raw_mut()?.abs_info(axis).map_err(map_input_err)?;
        Ok(AbsInfo {
            min: info.min as i32,
            max: info.max as i32,
            fuzz: info.fuzz as i32,
            flat: info.flat as i32,
            res: info.res as i32,
        })
    }

    fn enable_irq(&mut self) -> Result<(), InputError> {
        if self.raw.is_none() || self.irq_port.is_some() {
            return Err(InputError::NotAvailable);
        }
        self.irq_enabled.store(true, Ordering::Release);
        if let Some(lease) = &self.irq_lease
            && let Err(error) = lease.enable_binding_irq()
        {
            self.irq_enabled.store(false, Ordering::Release);
            return Err(InputError::Other(Box::new(error)));
        }
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), InputError> {
        self.irq_enabled.store(false, Ordering::Release);
        if let Some(lease) = &self.irq_lease {
            lease
                .disable_binding_irq()
                .map_err(|error| InputError::Other(Box::new(error)))?;
        }
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled.load(Ordering::Acquire)
    }

    fn take_irq_endpoint(&mut self) -> Option<InputIrqEndpoint> {
        let port = self.irq_port.take()?;
        let source_mask = self
            .irq_lease
            .as_ref()
            .and_then(crate::pci::PciIntxIrqLease::take_source_mask);
        Some(Box::new(VirtIoInputIrqEndpoint {
            port,
            enabled: Arc::clone(&self.irq_enabled),
            source_mask,
        }))
    }

    fn rearm_irq(&mut self, source: MaskedSource) -> Result<(), InputError> {
        let lease = self.irq_lease.as_ref().ok_or(InputError::NotSupported)?;
        self.irq_enabled.store(true, Ordering::Release);
        if !lease.rearm_source(source) {
            self.irq_enabled.store(false, Ordering::Release);
            return Err(InputError::NotAvailable);
        }
        Ok(())
    }
}

fn input_irq_event(status: InterruptStatus) -> IrqEvent {
    IrqEvent {
        handled: true,
        input_ready: status.contains(InterruptStatus::QUEUE_INTERRUPT),
    }
}

fn map_input_err(err: VirtIoError) -> InputError {
    match err {
        VirtIoError::Unsupported => InputError::NotSupported,
        VirtIoError::NotReady => InputError::Again,
        _ => InputError::Other(Box::new(err)),
    }
}

#[cfg(feature = "pci")]
fn pci_interrupt_port(
    endpoint: &rdrive::probe::pci::Endpoint,
) -> Result<VirtioInputInterruptPort, OnProbeError> {
    use rdrive::probe::pci::PciCapability;

    for capability in endpoint.capabilities() {
        let PciCapability::Vendor(address) = capability else {
            continue;
        };
        let header = endpoint.read(address.offset);
        if (header >> 24) as u8 != PCI_VIRTIO_ISR_CONFIG_TYPE {
            continue;
        }
        let capability_length = (header >> 16) as u8;
        if capability_length < PCI_VIRTIO_ISR_CAP_MIN_LENGTH {
            return Err(OnProbeError::other(
                "virtio PCI ISR capability is shorter than its fixed fields",
            ));
        }
        let bar = endpoint.read(address.offset + PCI_CAP_BAR_OFFSET) as u8;
        if bar >= 6 {
            return Err(OnProbeError::other(format!(
                "virtio PCI ISR capability names invalid BAR {bar}"
            )));
        }
        let region_offset = endpoint.read(address.offset + PCI_CAP_REGION_OFFSET) as usize;
        let region_length = endpoint.read(address.offset + PCI_CAP_REGION_LENGTH) as usize;
        if region_length == 0 {
            return Err(OnProbeError::other(
                "virtio PCI ISR capability has zero length",
            ));
        }
        let bar_range = endpoint.bar_mmio(bar).ok_or_else(|| {
            OnProbeError::other(format!("virtio PCI ISR capability names invalid BAR {bar}"))
        })?;
        let isr_phys = bar_range
            .start
            .checked_add(region_offset)
            .filter(|start| {
                start
                    .checked_add(region_length)
                    .is_some_and(|end| end <= bar_range.end)
            })
            .ok_or_else(|| OnProbeError::other("virtio PCI ISR capability exceeds its BAR"))?;
        let mapping = axklib::mmio::ioremap(isr_phys.into(), region_length)
            .map_err(|error| OnProbeError::other(format!("{error:?}")))?;
        return VirtioInputInterruptPort::from_pci_isr(mapping)
            .map_err(|error| OnProbeError::other(error.to_string()));
    }
    Err(OnProbeError::other(
        "virtio PCI transport has no ISR capability",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_irq_queue_interrupt_makes_input_ready() {
        assert_eq!(
            input_irq_event(InterruptStatus::QUEUE_INTERRUPT),
            IrqEvent {
                handled: true,
                input_ready: true,
            }
        );
    }

    #[test]
    fn input_irq_configuration_interrupt_is_claimed_without_input_ready() {
        assert_eq!(
            input_irq_event(InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT),
            IrqEvent {
                handled: true,
                input_ready: false,
            }
        );
    }
}
