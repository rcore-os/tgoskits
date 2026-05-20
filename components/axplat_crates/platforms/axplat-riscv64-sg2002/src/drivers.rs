use ax_plat::drivers::DriversIf;
use rdrive::probe::static_::StaticDeviceDesc;

use crate::config::devices;

static CVSD_REGS: &[(usize, usize)] = &[
    (devices::CVSD_PADDR, 0x1000),
    (devices::SYSCON_PADDR, 0x8000),
];

static STATIC_DEVICES: &[StaticDeviceDesc] = &[StaticDeviceDesc::new("cvsd").with_regs(CVSD_REGS)];

struct DriversIfImpl;

#[impl_plat_interface]
impl DriversIf for DriversIfImpl {
    fn static_devices_fn() -> &'static [StaticDeviceDesc] {
        STATIC_DEVICES
    }
}
