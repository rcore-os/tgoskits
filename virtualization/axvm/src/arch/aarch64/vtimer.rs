//! AArch64 virtual timer device contribution.
//!
//! The vtimer is architecture-owned topology: it is exposed to the common
//! device preparation path through a normal factory/bundle contribution, but
//! the concrete `arm_vgic` backend stays inside the AArch64 AxVM boundary
//! instead of leaking into the architecture-neutral `axdevice` crate.

use alloc::sync::Arc;

use arm_vgic::vtimer::{
    HostVtimerBackend, SysCntpCtlEl0, SysCntpTvalEl0, SysCntpctEl0, VtimerBackend, VtimerState,
};
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceLifecycle, DeviceManagerResult,
    DeviceRegistration, ServiceCardinality, ServiceKey,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

/// Factory for the standard AArch64 CNT* virtual-timer sysreg devices.
pub(crate) struct Aarch64VtimerFactory;

/// Service key for the AArch64 virtual-timer backend.
pub(crate) struct Aarch64VtimerBackendKey;

impl ServiceKey for Aarch64VtimerBackendKey {
    type Service = dyn VtimerBackend;

    const NAME: &'static str = "aarch64-vtimer-backend";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

struct VtimerLifecycle {
    state: Arc<VtimerState>,
    backend: Arc<dyn VtimerBackend>,
}

impl DeviceLifecycle for VtimerLifecycle {
    fn reset(&self) -> DeviceManagerResult {
        self.state.reset(self.backend.as_ref());
        Ok(())
    }

    fn suspend(&self) -> DeviceManagerResult {
        self.state.suspend(self.backend.as_ref());
        Ok(())
    }

    fn resume(&self) -> DeviceManagerResult {
        self.state.resume(Arc::clone(&self.backend));
        Ok(())
    }
}

impl Drop for VtimerLifecycle {
    fn drop(&mut self) {
        self.state.reset(self.backend.as_ref());
    }
}

impl Aarch64VtimerFactory {
    /// Builds the three CNT* system-register device contributions.
    fn build_bundle(&self) -> DeviceManagerResult<DeviceBundle> {
        let backend: Arc<dyn VtimerBackend> = Arc::new(HostVtimerBackend);
        let state = Arc::new(VtimerState::new());
        let lifecycle: Arc<dyn DeviceLifecycle> = Arc::new(VtimerLifecycle {
            state: Arc::clone(&state),
            backend: Arc::clone(&backend),
        });

        let bundle = DeviceBundle::from_registration(DeviceRegistration::Device(Arc::new(
            SysCntpCtlEl0::new(Arc::clone(&state), Arc::clone(&backend)),
        )))
        .with_registration(DeviceRegistration::Device(Arc::new(SysCntpctEl0::new(
            Arc::clone(&backend),
        ))))
        .with_registration(DeviceRegistration::Device(Arc::new(SysCntpTvalEl0::new(
            state,
            Arc::clone(&backend),
        ))));

        Ok(bundle
            .with_service::<Aarch64VtimerBackendKey>(backend)?
            .with_lifecycle(lifecycle))
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
