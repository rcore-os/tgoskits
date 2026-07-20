//! VirtIO GPU discovery split into owner-only control and IRQ capture parts.

extern crate alloc;

use alloc::{boxed::Box, format, string::ToString, sync::Arc};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use rdif_display::{
    DisplayError, DisplayExecution, DisplayInfo, DisplayIrqEndpoint, DisplayIrqFault, Event,
    FrameBuffer, PixelFormat,
};
use rdif_irq::{ContainmentCause, IrqCapture, IrqEndpoint, MaskedSource};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(feature = "pci")]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{Error as VirtIoError, device::gpu::VirtIOGpu, transport::InterruptStatus};

use crate::{
    BindingInfo, IrqBindingLease,
    display::PlatformDeviceDisplay,
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
    name: "VirtIO GPU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(feature = "pci")]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    crate::pci::ensure_virtio_pci_endpoint(probe.endpoint(), DeviceType::GPU)?;
    let info = binding_info_from_pci_endpoint(
        probe.info(),
        probe.endpoint(),
        PciIrqRequirement::Required,
    )?;
    let interrupt_port = pci_interrupt_port(probe.endpoint())?;
    let (transport, irq_lease) = crate::pci::take_virtio_display_transport(
        probe.endpoint_mut(),
        DeviceType::GPU,
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

/// Rejects a transport whose destructive interrupt-status capability was erased.
pub fn register_transport<T: VirtIoTransport>(
    _plat_dev: PlatformDevice,
    _transport: T,
) -> Result<(), OnProbeError> {
    Err(OnProbeError::other(
        "virtio display registration requires an independent interrupt port",
    ))
}

/// Registers a VirtIO display with a separately owned interrupt-status port.
pub fn register_transport_with_interrupt_port<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    interrupt_port: VirtioDisplayInterruptPort,
) -> Result<(), OnProbeError> {
    register_transport_with_parts(
        plat_dev,
        transport,
        interrupt_port,
        None,
        BindingInfo::empty(),
    )
}

/// Registers a statically discovered VirtIO MMIO display.
pub fn register_mmio_transport<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    mapping: mmio_api::Mmio,
) -> Result<(), OnProbeError> {
    let interrupt_port = VirtioDisplayInterruptPort::from_mmio(mapping)
        .map_err(|error| OnProbeError::other(error.to_string()))?;
    register_transport_with_interrupt_port(plat_dev, transport, interrupt_port)
}

fn register_transport_with_parts<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    interrupt_port: VirtioDisplayInterruptPort,
    irq_lease: Option<crate::pci::PciIntxIrqLease>,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let dev = VirtIoDisplay::discovered(transport, interrupt_port, irq_lease);
    let irq = plat_dev.register_display_with_info(dev, info);
    log::info!("discovered virtio GPU device irq={irq:?}");
    Ok(())
}

/// Destructive VirtIO interrupt status capability owned only by the IRQ action.
pub struct VirtioDisplayInterruptPort {
    registers: DisplayInterruptRegisters,
}

impl VirtioDisplayInterruptPort {
    /// Creates an MMIO endpoint while retaining the complete mapping lease.
    pub fn from_mmio(mapping: mmio_api::Mmio) -> Result<Self, DisplayError> {
        if mapping.size() < MMIO_INTERRUPT_REGISTERS_END {
            return Err(DisplayError::InvalidFramebuffer);
        }
        Ok(Self {
            registers: DisplayInterruptRegisters::Mmio(mapping),
        })
    }

    /// Creates a PCI endpoint from the transport's destructive ISR capability.
    pub fn from_pci_isr(mapping: mmio_api::Mmio) -> Result<Self, DisplayError> {
        if mapping.size() < size_of::<u8>() {
            return Err(DisplayError::NotAvailable);
        }
        Ok(Self {
            registers: DisplayInterruptRegisters::Pci(mapping),
        })
    }

    fn capture_status(&mut self) -> u32 {
        match &self.registers {
            DisplayInterruptRegisters::Mmio(mapping) => {
                let status = mapping.read::<u32>(MMIO_INTERRUPT_STATUS_OFFSET);
                if status != 0 {
                    mapping.write(MMIO_INTERRUPT_ACK_OFFSET, status);
                }
                status
            }
            // VirtIO PCI defines the ISR read itself as acknowledgement.
            DisplayInterruptRegisters::Pci(mapping) => u32::from(mapping.read::<u8>(0)),
        }
    }
}

enum DisplayInterruptRegisters {
    Mmio(mmio_api::Mmio),
    Pci(mmio_api::Mmio),
}

struct VirtIoDisplayIrqEndpoint {
    port: VirtioDisplayInterruptPort,
    enabled: Arc<AtomicBool>,
    source_mask: Option<crate::pci::PciIntxSourceMask>,
}

impl IrqEndpoint for VirtIoDisplayIrqEndpoint {
    type Event = Event;
    type Fault = DisplayIrqFault;

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
                Ok(source) => rdif_irq::FaultContainment::DeviceSourceMasked(source),
                Err(_) => rdif_irq::FaultContainment::Uncontained,
            };
            return IrqCapture::Fault {
                reason: DisplayIrqFault::InvalidStatus,
                containment,
            };
        }
        IrqCapture::Captured {
            event: display_irq_event(status),
            masked: None,
        }
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        let source_mask = self
            .source_mask
            .as_mut()
            .ok_or(DisplayIrqFault::Uncontained)?;
        self.enabled.store(false, Ordering::Release);
        Ok(source_mask.mask_from_irq())
    }
}

struct VirtIoDisplay<T: VirtIoTransport> {
    transport: Option<T>,
    raw: Option<VirtIOGpu<VirtIoHalImpl, T>>,
    info: Option<DisplayInfo>,
    fb_base: Option<NonNull<u8>>,
    irq_port: Option<VirtioDisplayInterruptPort>,
    irq_enabled: Arc<AtomicBool>,
    irq_lease: Option<crate::pci::PciIntxIrqLease>,
}

// SAFETY: the transport, MMIO leases, and framebuffer DMA allocation move as
// one device owner. After publication all mutable operations stay on its
// pinned maintenance thread; the framebuffer pointer is published separately.
unsafe impl<T: VirtIoTransport> Send for VirtIoDisplay<T> {}

impl<T: VirtIoTransport> VirtIoDisplay<T> {
    fn discovered(
        transport: T,
        irq_port: VirtioDisplayInterruptPort,
        irq_lease: Option<crate::pci::PciIntxIrqLease>,
    ) -> Self {
        Self {
            transport: Some(transport),
            raw: None,
            info: None,
            fb_base: None,
            irq_port: Some(irq_port),
            irq_enabled: Arc::new(AtomicBool::new(false)),
            irq_lease,
        }
    }

    fn raw_mut(&mut self) -> Result<&mut VirtIOGpu<VirtIoHalImpl, T>, DisplayError> {
        self.raw.as_mut().ok_or(DisplayError::NotAvailable)
    }
}

impl<T: VirtIoTransport> DriverGeneric for VirtIoDisplay<T> {
    fn name(&self) -> &str {
        "virtio-gpu"
    }
}

impl<T: VirtIoTransport> rdif_display::Interface for VirtIoDisplay<T> {
    fn initialize(&mut self) -> Result<(), DisplayError> {
        if self.raw.is_some() {
            return Ok(());
        }
        let transport = self.transport.take().ok_or(DisplayError::NotAvailable)?;
        let mut raw = VirtIOGpu::new(transport).map_err(map_display_err)?;
        let framebuffer = raw.setup_framebuffer().map_err(map_display_err)?;
        let fb_base =
            NonNull::new(framebuffer.as_mut_ptr()).ok_or(DisplayError::InvalidFramebuffer)?;
        let fb_size = framebuffer.len();
        let (width, height) = raw.resolution().map_err(map_display_err)?;
        self.info = Some(DisplayInfo {
            width,
            height,
            stride: width as usize * 4,
            format: PixelFormat::Xrgb8888,
            fb_size,
        });
        self.fb_base = Some(fb_base);
        self.raw = Some(raw);
        Ok(())
    }

    fn execution(&self) -> DisplayExecution {
        DisplayExecution::Interrupt
    }

    fn info(&self) -> Result<DisplayInfo, DisplayError> {
        self.info.ok_or(DisplayError::NotAvailable)
    }

    fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError> {
        let info = self.info.ok_or(DisplayError::NotAvailable)?;
        let base = self.fb_base.ok_or(DisplayError::InvalidFramebuffer)?;
        Ok(unsafe { FrameBuffer::from_raw_parts_mut(base.as_ptr(), info.fb_size) })
    }

    fn need_flush(&self) -> bool {
        true
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        self.raw_mut()?.flush().map_err(map_display_err)
    }

    fn enable_irq(&mut self) -> Result<(), DisplayError> {
        self.irq_enabled.store(true, Ordering::Release);
        if let Some(lease) = &self.irq_lease
            && let Err(error) = lease.enable_binding_irq()
        {
            self.irq_enabled.store(false, Ordering::Release);
            return Err(DisplayError::Other(Box::new(error)));
        }
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), DisplayError> {
        self.irq_enabled.store(false, Ordering::Release);
        if let Some(lease) = &self.irq_lease {
            lease
                .disable_binding_irq()
                .map_err(|error| DisplayError::Other(Box::new(error)))?;
        }
        Ok(())
    }

    fn take_irq_endpoint(&mut self) -> Option<DisplayIrqEndpoint> {
        let port = self.irq_port.take()?;
        let source_mask = self
            .irq_lease
            .as_ref()
            .and_then(crate::pci::PciIntxIrqLease::take_source_mask);
        Some(Box::new(VirtIoDisplayIrqEndpoint {
            port,
            enabled: Arc::clone(&self.irq_enabled),
            source_mask,
        }))
    }

    fn service_irq(&mut self, _event: Event) -> Result<(), DisplayError> {
        Ok(())
    }

    fn rearm_irq(&mut self, source: MaskedSource) -> Result<(), DisplayError> {
        let lease = self.irq_lease.as_ref().ok_or(DisplayError::NotSupported)?;
        self.irq_enabled.store(true, Ordering::Release);
        if !lease.rearm_source(source) {
            self.irq_enabled.store(false, Ordering::Release);
            return Err(DisplayError::NotAvailable);
        }
        Ok(())
    }
}

fn display_irq_event(status: InterruptStatus) -> Event {
    Event {
        handled: true,
        changed: status.contains(InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT),
    }
}

fn map_display_err(err: VirtIoError) -> DisplayError {
    match err {
        VirtIoError::Unsupported => DisplayError::NotSupported,
        VirtIoError::NotReady => DisplayError::NotAvailable,
        _ => DisplayError::Other(Box::new(err)),
    }
}

#[cfg(feature = "pci")]
fn pci_interrupt_port(
    endpoint: &rdrive::probe::pci::Endpoint,
) -> Result<VirtioDisplayInterruptPort, OnProbeError> {
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
        return VirtioDisplayInterruptPort::from_pci_isr(mapping)
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
    fn display_irq_reports_configuration_changes() {
        assert_eq!(
            display_irq_event(InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT),
            Event {
                handled: true,
                changed: true,
            }
        );
    }

    #[test]
    fn display_irq_reports_queue_interrupt_as_handled_only() {
        assert_eq!(
            display_irq_event(InterruptStatus::QUEUE_INTERRUPT),
            Event {
                handled: true,
                changed: false,
            }
        );
    }
}
