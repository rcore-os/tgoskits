use ax_plat::drivers::DriversIf;
use rdrive::probe::static_::{StaticDeviceDesc, StaticPciEcam};

use crate::config::devices;

const PCI_ECAM_SIZE: usize = (devices::PCI_BUS_END + 1) << 20;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[StaticDeviceDesc::new("pci-ecam")
    .with_pci_ecam(StaticPciEcam::new(devices::PCI_ECAM_BASE, PCI_ECAM_SIZE))];

struct DriversIfImpl;

#[impl_plat_interface]
impl DriversIf for DriversIfImpl {
    fn static_devices_fn() -> &'static [StaticDeviceDesc] {
        STATIC_DEVICES
    }
}
