//! Static platform driver resources.

use rdrive::probe::static_::StaticDeviceDesc;

/// Static platform driver resource interface.
///
/// Static platforms describe probe inputs here. Driver implementations are
/// registered through `.driver.register*` and consume these resources through
/// `ProbeKind::Static`.
#[def_plat_interface]
pub trait DriversIf {
    /// Returns the static device descriptors exposed by the platform.
    fn static_devices_fn() -> &'static [StaticDeviceDesc];
}

/// Returns the static device descriptors exposed by the platform.
pub fn static_devices() -> &'static [StaticDeviceDesc] {
    crate::__priv::call_interface!(DriversIf::static_devices_fn)
}
