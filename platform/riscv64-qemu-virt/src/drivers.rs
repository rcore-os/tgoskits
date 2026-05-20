use ax_plat::drivers::DriversIf;
use rdrive::probe::static_::StaticDeviceDesc;

use crate::config::devices;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[StaticDeviceDesc::new("virtio-mmio")
    .with_regs(devices::VIRTIO_MMIO_RANGES)
    .with_probe_each_reg()];

struct DriversIfImpl;

#[impl_plat_interface]
impl DriversIf for DriversIfImpl {
    fn static_devices_fn() -> &'static [StaticDeviceDesc] {
        STATIC_DEVICES
    }
}
