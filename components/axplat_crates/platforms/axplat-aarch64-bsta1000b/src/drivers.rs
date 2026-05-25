use ax_plat::drivers::DriversIf;
use rdrive::probe::static_::StaticDeviceDesc;

struct DriversIfImpl;

#[impl_plat_interface]
impl DriversIf for DriversIfImpl {
    fn static_devices_fn() -> &'static [StaticDeviceDesc] {
        &[]
    }
}
