extern crate alloc;

use alloc::format;

use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, static_::StaticInfo},
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};
use virtio_drivers::transport::{DeviceType, Transport};

use crate::virtio::{self, VirtIoHalImpl};

pub const REGISTER: DriverRegister = DriverRegister {
    name: "Static VirtIO Block",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[
        ProbeKind::Static {
            on_probe: probe_mmio,
        },
        #[cfg(feature = "bus-pci")]
        ProbeKind::Pci {
            on_probe: probe_pci,
        },
    ],
};

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
        if ty == DeviceType::Block {
            return register_block(plat_dev, transport);
        }
    }

    Err(OnProbeError::NotMatch)
}

#[cfg(feature = "bus-pci")]
fn probe_pci(
    endpoint: &mut rdrive::probe::pci::EndpointRc,
    plat_dev: PlatformDevice,
) -> Result<(), OnProbeError> {
    let transport = crate::pci::take_virtio_transport(endpoint, DeviceType::Block)?;
    register_block(plat_dev, transport)
}

fn register_block<T: Transport + 'static>(
    plat_dev: PlatformDevice,
    transport: T,
) -> Result<(), OnProbeError> {
    let driver = ax_driver_virtio::VirtIoBlkDev::<VirtIoHalImpl, T>::try_new(transport)
        .map_err(|err| OnProbeError::other(format!("failed to initialize virtio-blk: {err:?}")))?;
    crate::block::register_block(plat_dev, driver);
    log::info!("registered static virtio block device");
    Ok(())
}
