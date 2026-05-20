#[cfg(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
extern crate alloc;

#[cfg(feature = "pci")]
use ax_drivers::pci;
#[cfg(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
use ax_drivers::virtio;
#[cfg(any(
    feature = "pci",
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
use rdrive::PlatformDevice;
#[cfg(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
use rdrive::probe::OnProbeError;
#[cfg(feature = "pci")]
use rdrive::probe::pci::{PciMem32, PciMem64};
use rdrive::{Platform, probe::static_::StaticDeviceDesc};
#[cfg(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
use virtio_drivers::transport::DeviceType;

#[cfg(any(
    feature = "pci",
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
use crate::config::devices;

mod registers;

#[cfg(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
const VIRTIO_MMIO: &str = "virtio-mmio";
static STATIC_DEVICES: &[StaticDeviceDesc] = &[
    #[cfg(any(
        feature = "virtio-blk",
        feature = "virtio-net",
        feature = "virtio-gpu",
        feature = "virtio-input",
        feature = "virtio-socket"
    ))]
    StaticDeviceDesc::new(VIRTIO_MMIO),
    #[cfg(feature = "pci")]
    StaticDeviceDesc::new(pci::DEVICE_NAME),
];

pub(super) fn init() {
    rdrive::init(Platform::Static(STATIC_DEVICES))
        .unwrap_or_else(|err| panic!("failed to initialize static rdrive source: {err:?}"));
    registers::append_linker_registers();
    #[cfg(feature = "pci")]
    register_pcie();
    rdrive::probe_pre_kernel()
        .unwrap_or_else(|err| panic!("failed to run static pre-kernel probes: {err:?}"));
    #[cfg(any(
        feature = "virtio-blk",
        feature = "virtio-net",
        feature = "virtio-gpu",
        feature = "virtio-input",
        feature = "virtio-socket"
    ))]
    register_virtio_mmio_devices();
}

#[cfg(feature = "pci")]
fn register_pcie() {
    let ecam_size = (devices::PCI_BUS_END + 1) << 20;
    let mem32 = pci_mem32_from_config();
    let mem64 = pci_mem64_from_config();
    pci::register_ecam_controller(
        static_descriptor(pci::DEVICE_NAME),
        devices::PCI_ECAM_BASE,
        ecam_size,
        mem32,
        mem64,
    )
    .unwrap_or_else(|err| panic!("failed to register static PCIe controller: {err:?}"));
}

#[cfg(feature = "pci")]
fn pci_mem32_from_config() -> Option<PciMem32> {
    let (address, size) = devices::PCI_RANGES.get(1).copied()?;
    if size == 0 {
        return None;
    }
    Some(PciMem32 {
        address: u32::try_from(address).ok()?,
        size: u32::try_from(size).ok()?,
    })
}

#[cfg(feature = "pci")]
fn pci_mem64_from_config() -> Option<PciMem64> {
    let (address, size) = devices::PCI_RANGES.get(2).copied()?;
    if size == 0 || usize::BITS <= 32 {
        return None;
    }
    Some(PciMem64 {
        address: address as u64,
        size: size as u64,
    })
}

#[cfg(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
fn register_virtio_mmio_devices() {
    for (base, size) in devices::VIRTIO_MMIO_RANGES.iter().copied() {
        let descriptor = static_descriptor(VIRTIO_MMIO);
        if let Err(err) = register_virtio_mmio(descriptor, base, size) {
            match err {
                OnProbeError::NotMatch => {}
                other => log::warn!("failed to register virtio-mmio {base:#x}: {other}"),
            }
        }
    }
}

#[cfg(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
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
    register_virtio_transport(plat_dev, ty, transport)
}

#[cfg(any(
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
fn register_virtio_transport<T: virtio_drivers::transport::Transport + 'static>(
    plat_dev: PlatformDevice,
    ty: DeviceType,
    transport: T,
) -> Result<(), OnProbeError> {
    match ty {
        #[cfg(feature = "virtio-blk")]
        DeviceType::Block => virtio::block::register_transport(plat_dev, transport),
        #[cfg(feature = "virtio-net")]
        DeviceType::Network => virtio::net::register_transport(plat_dev, transport),
        #[cfg(feature = "virtio-gpu")]
        DeviceType::GPU => virtio::display::register_transport(plat_dev, transport),
        #[cfg(feature = "virtio-input")]
        DeviceType::Input => virtio::input::register_transport(plat_dev, transport),
        #[cfg(feature = "virtio-socket")]
        DeviceType::Socket => virtio::vsock::register_transport(plat_dev, transport),
        _ => Err(OnProbeError::NotMatch),
    }
}

#[cfg(any(
    feature = "pci",
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
fn static_descriptor(name: &'static str) -> PlatformDevice {
    let mut descriptor = rdrive::Descriptor::new();
    descriptor.name = name;
    PlatformDevice { descriptor }
}
