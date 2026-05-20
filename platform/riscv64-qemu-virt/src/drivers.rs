use ax_plat::drivers::DriversIf;
use rdrive::probe::static_::StaticDeviceDesc;

#[cfg(feature = "virtio-blk")]
use crate::config::devices;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[
    #[cfg(feature = "virtio-blk")]
    StaticDeviceDesc::new("virtio-mmio")
        .with_regs(devices::VIRTIO_MMIO_RANGES)
        .with_probe_each_reg(),
];

struct DriversIfImpl;

#[impl_plat_interface]
impl DriversIf for DriversIfImpl {
    fn static_devices_fn() -> &'static [StaticDeviceDesc] {
        STATIC_DEVICES
    }
}
