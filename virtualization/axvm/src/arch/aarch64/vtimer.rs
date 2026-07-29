//! AArch64 virtual timer device contribution.
//!
//! The vtimer is architecture-owned topology: it is exposed to the common
//! device preparation path through a normal factory/bundle contribution, but
//! the concrete `arm_vgic` backend stays inside the AArch64 AxVM boundary
//! instead of leaking into the architecture-neutral `axdevice` crate.

use arm_vgic::vtimer::get_sysreg_device;
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceManagerResult, DeviceRegistration,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

/// Factory for the standard AArch64 CNT* virtual-timer sysreg devices.
pub(crate) struct Aarch64VtimerFactory;

impl Aarch64VtimerFactory {
    /// Builds the CNT* system-register device contributions.
    fn build_bundle(&self) -> DeviceManagerResult<DeviceBundle> {
        let mut bundle = DeviceBundle::new();
        for device in get_sysreg_device() {
            bundle.push(DeviceRegistration::Device(device));
        }
        Ok(bundle)
    }
}

impl DeviceFactory for Aarch64VtimerFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Aarch64Vtimer
    }

    fn build(
        &self,
        _config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        self.build_bundle()
    }
}
