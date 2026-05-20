#[cfg(feature = "virtio-blk")]
extern crate alloc;

#[cfg(feature = "virtio-blk")]
use ax_drivers::virtio;
use rdrive::{Platform, probe::static_::StaticDeviceDesc};
#[cfg(feature = "virtio-blk")]
use rdrive::{PlatformDevice, probe::OnProbeError};
#[cfg(feature = "virtio-blk")]
use virtio_drivers::transport::DeviceType;

#[cfg(feature = "virtio-blk")]
use crate::config::devices;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[
    #[cfg(feature = "virtio-blk")]
    StaticDeviceDesc::new(virtio::MMIO_DEVICE_NAME),
];

pub(crate) fn init() {
    rdrive::init(Platform::Static(STATIC_DEVICES))
        .unwrap_or_else(|err| panic!("failed to initialize static rdrive source: {err:?}"));
    rdrive::probe_pre_kernel()
        .unwrap_or_else(|err| panic!("failed to run static pre-kernel probes: {err:?}"));
    #[cfg(feature = "virtio-blk")]
    register_virtio_mmio_devices();
}

#[cfg(feature = "virtio-blk")]
fn register_virtio_mmio_devices() {
    for (base, size) in devices::VIRTIO_MMIO_RANGES.iter().copied() {
        let descriptor = static_descriptor(virtio::MMIO_DEVICE_NAME);
        if let Err(err) = register_virtio_mmio(descriptor, base, size) {
            match err {
                OnProbeError::NotMatch => {}
                other => log::warn!("failed to register virtio-mmio {base:#x}: {other}"),
            }
        }
    }
}

#[cfg(feature = "virtio-blk")]
fn register_virtio_mmio(
    plat_dev: PlatformDevice,
    base: usize,
    size: usize,
) -> Result<(), OnProbeError> {
    let mmio = axklib::mmio::ioremap_raw(base.into(), size)
        .map_err(|err| OnProbeError::other(alloc::format!("failed to map virtio-mmio: {err:?}")))?;
    let Some((ty, transport)) = virtio::probe_mmio_device(mmio.as_ptr(), size) else {
        return Err(OnProbeError::NotMatch);
    };
    if ty != DeviceType::Block {
        return Err(OnProbeError::NotMatch);
    }
    virtio::block::register_transport(plat_dev, transport)
}

#[cfg(feature = "virtio-blk")]
fn static_descriptor(name: &'static str) -> PlatformDevice {
    let mut descriptor = rdrive::Descriptor::new();
    descriptor.name = name;
    PlatformDevice { descriptor }
}
