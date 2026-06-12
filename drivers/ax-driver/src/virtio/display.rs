extern crate alloc;

use alloc::format;

use rdif_display::{DisplayError, DisplayInfo, FrameBuffer, PixelFormat};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(all(feature = "pci", any(plat_static, plat_dyn)))]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{Error as VirtIoError, device::gpu::VirtIOGpu, transport::Transport};

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
    let transport = crate::pci::take_virtio_transport(probe.endpoint_mut(), DeviceType::GPU)?;
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
    let dev = VirtIoDisplay::new(transport)
        .map_err(|err| OnProbeError::other(format!("failed to initialize virtio-gpu: {err:?}")))?;
    let irq = plat_dev.register_display_with_info(dev, info);
    log::info!("registered virtio GPU device irq={irq:?}");
    Ok(())
}

struct VirtIoDisplay<T: Transport + 'static> {
    raw: VirtIOGpu<VirtIoHalImpl, T>,
    info: DisplayInfo,
    fb_base: *mut u8,
}

unsafe impl<T: Transport + 'static> Send for VirtIoDisplay<T> {}

impl<T: Transport + 'static> VirtIoDisplay<T> {
    fn new(transport: T) -> Result<Self, VirtIoError> {
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
        Ok(Self { raw, info, fb_base })
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

    fn need_flush(&self) -> bool {
        true
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        self.raw.flush().map_err(map_display_err)
    }
}

fn map_display_err(err: VirtIoError) -> DisplayError {
    match err {
        VirtIoError::Unsupported => DisplayError::NotSupported,
        VirtIoError::NotReady => DisplayError::NotAvailable,
        _ => DisplayError::Other(alloc::boxed::Box::new(err)),
    }
}
