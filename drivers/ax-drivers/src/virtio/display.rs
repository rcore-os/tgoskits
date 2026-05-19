extern crate alloc;

use alloc::format;

use rdif_display::{DisplayError, DisplayInfo, FrameBuffer, PixelFormat};
#[cfg(probe = "static-mmio")]
use rdrive::probe::static_::StaticInfo;
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
use virtio_drivers::{
    Error as VirtIoError,
    device::gpu::VirtIOGpu,
    transport::{DeviceType, Transport},
};

#[cfg(probe = "static-mmio")]
use crate::virtio;
use crate::{bindings::display::PlatformDeviceDisplay, virtio::VirtIoHalImpl};

crate::register_driver!(
    name: "VirtIO GPU",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        #[cfg(probe = "static-mmio")]
        ProbeKind::Static {
            on_probe: probe_mmio,
        },
        #[cfg(probe = "pci")]
        ProbeKind::Pci {
            on_probe: probe_pci,
        },
    ],
);

#[cfg(probe = "static-mmio")]
fn probe_mmio(info: StaticInfo, plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    if info.name() != virtio::MMIO_DEVICE_NAME {
        return Err(OnProbeError::NotMatch);
    }

    for (base, size) in ax_config::devices::VIRTIO_MMIO_RANGES {
        let mmio = axklib::mmio::ioremap_raw((*base).into(), *size)
            .map_err(|err| OnProbeError::other(format!("failed to map virtio-mmio: {err:?}")))?;
        let Some((ty, transport)) = virtio::probe_mmio_device(mmio.as_ptr(), *size) else {
            continue;
        };
        if ty == DeviceType::GPU {
            return register_display(plat_dev, transport);
        }
    }

    Err(OnProbeError::NotMatch)
}

#[cfg(probe = "pci")]
fn probe_pci(
    endpoint: &mut rdrive::probe::pci::EndpointRc,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
    let transport = crate::pci::take_virtio_transport(endpoint, DeviceType::GPU)?;
    register_display(plat_dev, transport)
}

fn register_display<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let dev = VirtIoDisplay::new(transport)
        .map_err(|err| OnProbeError::other(format!("failed to initialize virtio-gpu: {err:?}")))?;
    plat_dev.register_display(dev);
    log::info!("registered virtio GPU device");
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
