use ax_plat::drivers::DriversIf;
use rdrive::probe::static_::StaticDeviceDesc;

use crate::config::devices;

static SDMMC_REGS: &[(usize, usize)] = &[(devices::SDMMC_PADDR, 0x1000)];

static STATIC_DEVICES: &[StaticDeviceDesc] =
    &[StaticDeviceDesc::new("sdmmc").with_regs(SDMMC_REGS)];

struct DriversIfImpl;

#[impl_plat_interface]
impl DriversIf for DriversIfImpl {
    fn static_devices_fn() -> &'static [StaticDeviceDesc] {
        STATIC_DEVICES
    }
}
