//! AArch64 virtual timer device contribution.
//!
//! The vtimer is architecture-owned topology: it is exposed to the common
//! device preparation path through a normal factory/bundle contribution, but
//! the concrete `arm_vgic` backend stays inside the AArch64 AxVM boundary
//! instead of leaking into the architecture-neutral `axdevice` crate.

use alloc::sync::Arc;

use arm_vgic::vtimer::{VtimerState, get_sysreg_device_with_state};
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceLifecycle, DeviceManagerResult,
    DeviceRegistration,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

/// Factory for the standard AArch64 CNT* virtual-timer sysreg devices.
pub(crate) struct Aarch64VtimerFactory;

struct VtimerLifecycle {
    state: Arc<VtimerState>,
}

impl DeviceLifecycle for VtimerLifecycle {
    fn reset(&self) -> DeviceManagerResult {
        self.state.reset();
        Ok(())
    }

    fn suspend(&self) -> DeviceManagerResult {
        self.state.suspend();
        Ok(())
    }

    fn resume(&self) -> DeviceManagerResult {
        self.state.resume();
        Ok(())
    }
}

impl Drop for VtimerLifecycle {
    fn drop(&mut self) {
        self.state.reset();
    }
}

impl Aarch64VtimerFactory {
    /// Builds the CNT* system-register device contributions.
    fn build_bundle(&self) -> DeviceManagerResult<DeviceBundle> {
        let (state, devices) = get_sysreg_device_with_state();
        let lifecycle: Arc<dyn DeviceLifecycle> = Arc::new(VtimerLifecycle { state });
        let mut bundle = DeviceBundle::new();
        for device in devices {
            bundle.push(DeviceRegistration::Device(device));
        }
        Ok(bundle.with_lifecycle(lifecycle))
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
