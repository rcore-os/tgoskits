use ax_plat::drivers::DriversIf;
use rdrive::probe::static_::StaticDeviceDesc;
#[cfg(feature = "pci")]
use rdrive::probe::static_::StaticPciEcam;

#[cfg(any(
    feature = "pci",
    feature = "virtio-blk",
    feature = "virtio-net",
    feature = "virtio-gpu",
    feature = "virtio-input",
    feature = "virtio-socket"
))]
use crate::config::devices;

#[cfg(feature = "pci")]
const PCI_ECAM_SIZE: usize = (devices::PCI_BUS_END + 1) << 20;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[
    #[cfg(any(
        feature = "virtio-blk",
        feature = "virtio-net",
        feature = "virtio-gpu",
        feature = "virtio-input",
        feature = "virtio-socket"
    ))]
    StaticDeviceDesc::new("virtio-mmio")
        .with_regs(devices::VIRTIO_MMIO_RANGES)
        .with_probe_each_reg(),
    #[cfg(feature = "pci")]
    StaticDeviceDesc::new("pci-ecam").with_pci_ecam(
        StaticPciEcam::new(devices::PCI_ECAM_BASE, PCI_ECAM_SIZE).with_ranges(devices::PCI_RANGES),
    ),
];

struct DriversIfImpl;

#[impl_plat_interface]
impl DriversIf for DriversIfImpl {
    fn static_devices_fn() -> &'static [StaticDeviceDesc] {
        STATIC_DEVICES
    }
}
