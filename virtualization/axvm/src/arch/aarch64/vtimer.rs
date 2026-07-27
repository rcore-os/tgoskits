//! AArch64 virtual timer device contribution.
//!
//! The vtimer is architecture-owned topology: it is exposed to the common
//! device preparation path through a normal factory/bundle contribution, but
//! the concrete `arm_vgic` backend stays inside the AArch64 AxVM boundary
//! instead of leaking into the architecture-neutral `axdevice` crate.

use alloc::sync::Arc;

use arm_vgic::vtimer::new_sysreg_devices;
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceManagerResult, DeviceRegistration,
};
use axdevice_base::{Device, SysRegDeviceAdapter};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

/// Factory for the standard AArch64 CNT* virtual-timer sysreg devices.
pub(crate) struct Aarch64VtimerFactory;

impl Aarch64VtimerFactory {
    /// Builds the CNT* system-register device contributions.
    fn build_bundle(&self) -> DeviceManagerResult<DeviceBundle> {
        let (cval, ctl, counter, tval) = new_sysreg_devices();
        let cval: Arc<dyn Device> = Arc::new(SysRegDeviceAdapter::new(cval));
        let ctl: Arc<dyn Device> = Arc::new(SysRegDeviceAdapter::new(ctl));
        let counter: Arc<dyn Device> = Arc::new(SysRegDeviceAdapter::new(counter));
        let tval: Arc<dyn Device> = Arc::new(SysRegDeviceAdapter::new(tval));

        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(cval))
            .with_registration(DeviceRegistration::Device(ctl))
            .with_registration(DeviceRegistration::Device(counter))
            .with_registration(DeviceRegistration::Device(tval)))
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
