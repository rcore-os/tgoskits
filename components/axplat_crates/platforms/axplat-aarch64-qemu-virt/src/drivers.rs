use ax_plat::drivers::DriversIf;
use rdrive::probe::static_::{StaticDeviceDesc, StaticPciEcam};

use crate::config::devices;

const PCI_ECAM_SIZE: usize = (devices::PCI_BUS_END + 1) << 20;
const PCI_LEGACY_IRQS: &[usize] = &[35, 36, 37, 38];

static STATIC_DEVICES: &[StaticDeviceDesc] = &[
    StaticDeviceDesc::new("virtio-mmio")
        .with_regs(devices::VIRTIO_MMIO_RANGES)
        .with_probe_each_reg(),
    StaticDeviceDesc::new("pci-ecam")
        .with_irqs(PCI_LEGACY_IRQS)
        .with_pci_ecam(
            StaticPciEcam::new(devices::PCI_ECAM_BASE, PCI_ECAM_SIZE)
                .with_ranges(devices::PCI_RANGES),
        ),
];

struct DriversIfImpl;

#[impl_plat_interface]
impl DriversIf for DriversIfImpl {
    fn static_devices_fn() -> &'static [StaticDeviceDesc] {
        STATIC_DEVICES
    }
}
