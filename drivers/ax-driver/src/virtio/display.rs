extern crate alloc;

use alloc::format;

use rdif_display::{DisplayError, DisplayInfo, Event, FrameBuffer, PixelFormat};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    Error as VirtIoError,
    device::gpu::VirtIOGpu,
    transport::{InterruptStatus, Transport},
};

use crate::{BindingInfo, display::PlatformDeviceDisplay, virtio::VirtIoHalImpl};
#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
use crate::{PciIrqRequirement, binding_info_from_pci};

#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
crate::model_register!(
    name: "VirtIO GPU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    let transport =
        crate::pci::take_virtio_transport_masked(probe.endpoint_mut(), DeviceType::GPU)?;
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Optional)?;
    register_transport_with_info(probe.into_platform_device(), transport, info)
}

pub fn register_transport<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    register_transport_with_info(plat_dev, transport, BindingInfo::empty())
}

pub fn register_transport_with_info<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
    info: BindingInfo,
) -> Result<(), OnProbeError> {
    let irq_num = info.irq_num();
    let dev = VirtIoDisplay::new(transport, irq_num)
        .map_err(|err| OnProbeError::other(format!("failed to initialize virtio-gpu: {err:?}")))?;
    let irq = plat_dev.register_display_with_info(dev, info);
    log::info!("registered virtio GPU device irq={irq:?}");
    Ok(())
}

struct VirtIoDisplay<T: Transport + 'static> {
    raw: VirtIOGpu<VirtIoHalImpl, T>,
    info: DisplayInfo,
    fb_base: *mut u8,
    irq_num: Option<usize>,
    irq_enabled: bool,
}

unsafe impl<T: Transport + 'static> Send for VirtIoDisplay<T> {}

impl<T: Transport + 'static> VirtIoDisplay<T> {
    fn new(transport: T, irq_num: Option<usize>) -> Result<Self, VirtIoError> {
        let mut raw = VirtIOGpu::new(transport)?;
        let framebuffer = raw.setup_framebuffer()?;
        let fb_base = framebuffer.as_mut_ptr();
        let fb_size = framebuffer.len();
        let (width, height) = raw.resolution()?;
        let info = DisplayInfo {
            width,
            height,
            stride: width as usize * 4,
            format: PixelFormat::Xrgb8888,
            fb_size,
        };
        let _ = raw.ack_interrupt();
        Ok(Self {
            raw,
            info,
            fb_base,
            irq_num,
            irq_enabled: false,
        })
    }
}

impl<T: Transport + 'static> DriverGeneric for VirtIoDisplay<T> {
    fn name(&self) -> &str {
        "virtio-gpu"
    }
}

impl<T: Transport + 'static> rdif_display::Interface for VirtIoDisplay<T> {
    fn info(&self) -> DisplayInfo {
        self.info
    }

    fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError> {
        Ok(unsafe { FrameBuffer::from_raw_parts_mut(self.fb_base, self.info.fb_size) })
    }

    fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }

    fn need_flush(&self) -> bool {
        true
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        self.raw.flush().map_err(map_display_err)
    }

    fn enable_irq(&mut self) {
        self.irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let status = self.raw.ack_interrupt();
        display_irq_event(self.irq_enabled, status)
    }
}

fn display_irq_event(irq_enabled: bool, status: InterruptStatus) -> Event {
    if !irq_enabled {
        return Event::none();
    }
    Event {
        handled: !status.is_empty(),
        changed: status.contains(InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT),
    }
}

fn map_display_err(err: VirtIoError) -> DisplayError {
    match err {
        VirtIoError::Unsupported => DisplayError::NotSupported,
        VirtIoError::NotReady => DisplayError::NotAvailable,
        _ => DisplayError::Other(alloc::boxed::Box::new(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_irq_is_ignored_until_driver_enables_it() {
        let status =
            InterruptStatus::QUEUE_INTERRUPT | InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT;

        assert_eq!(display_irq_event(false, status), Event::none());
    }

    #[test]
    fn display_irq_reports_configuration_changes() {
        assert_eq!(
            display_irq_event(true, InterruptStatus::DEVICE_CONFIGURATION_INTERRUPT),
            Event {
                handled: true,
                changed: true,
            }
        );
    }

    #[test]
    fn display_irq_reports_non_configuration_interrupt_as_handled_only() {
        assert_eq!(
            display_irq_event(true, InterruptStatus::QUEUE_INTERRUPT),
            Event {
                handled: true,
                changed: false,
            }
        );
    }

    #[test]
    fn display_irq_empty_status_is_not_claimed() {
        assert_eq!(
            display_irq_event(true, InterruptStatus::empty()),
            Event::none()
        );
    }
}
