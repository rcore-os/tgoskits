use ax_plat::drivers::DriversIf;
use rdrive::probe::static_::StaticDeviceDesc;

#[cfg(feature = "sdmmc")]
use crate::config::devices;

#[cfg(feature = "sdmmc")]
static SDMMC_REGS: &[(usize, usize)] = &[(devices::SDMMC_PADDR, 0x1000)];

static STATIC_DEVICES: &[StaticDeviceDesc] = &[
    #[cfg(feature = "sdmmc")]
    StaticDeviceDesc::new("sdmmc").with_regs(SDMMC_REGS),
];

struct DriversIfImpl;

#[impl_plat_interface]
impl DriversIf for DriversIfImpl {
    fn static_devices_fn() -> &'static [StaticDeviceDesc] {
        STATIC_DEVICES
    }
}
