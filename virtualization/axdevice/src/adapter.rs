// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Migration helpers that depend on concrete device-crate types.
//!
//! These will move out of `axdevice` as the migration progresses.

// ---------------------------------------------------------------------------
// vtimer helper (will move to axvm — it depends on arm_vgic concrete types)
// ---------------------------------------------------------------------------

/// Creates the standard vTimer device contribution.
///
/// The vTimer is architecture-owned boot-time topology rather than one static
/// `EmulatedDeviceConfig`, so it exposes a small typed factory instead of
/// being registered directly from the AArch64 VM module.
#[cfg(target_arch = "aarch64")]
pub struct Aarch64VtimerFactory;

/// Capability used by AArch64 timer devices to inject a virtual interrupt.
#[cfg(target_arch = "aarch64")]
pub trait InterruptInjectionPort: Send + Sync {
    /// Injects `vector` into the selected VM vCPU.
    fn inject(&self, target: arm_vgic::vtimer::VtimerTarget, vector: u8);
}

/// Service key for the AArch64 virtual-timer backend.
#[cfg(target_arch = "aarch64")]
pub struct Aarch64VtimerBackendKey;

#[cfg(target_arch = "aarch64")]
impl crate::ServiceKey for Aarch64VtimerBackendKey {
    type Service = dyn arm_vgic::vtimer::VtimerBackend;

    const NAME: &'static str = "aarch64-vtimer-backend";
    const CARDINALITY: crate::ServiceCardinality = crate::ServiceCardinality::Single;
}

/// Service key for architecture-owned virtual interrupt injection.
#[cfg(target_arch = "aarch64")]
pub struct InterruptInjectionPortKey;

#[cfg(target_arch = "aarch64")]
impl crate::ServiceKey for InterruptInjectionPortKey {
    type Service = dyn InterruptInjectionPort;

    const NAME: &'static str = "aarch64-interrupt-injection";
    const CARDINALITY: crate::ServiceCardinality = crate::ServiceCardinality::Single;
}

#[cfg(target_arch = "aarch64")]
struct VtimerInterruptInjectionPort {
    backend: alloc::sync::Arc<dyn arm_vgic::vtimer::VtimerBackend>,
}

#[cfg(target_arch = "aarch64")]
struct VtimerLifecycle {
    state: alloc::sync::Arc<arm_vgic::vtimer::VtimerState>,
    backend: alloc::sync::Arc<dyn arm_vgic::vtimer::VtimerBackend>,
}

#[cfg(target_arch = "aarch64")]
impl crate::DeviceLifecycle for VtimerLifecycle {
    fn reset(&self) -> crate::DeviceManagerResult {
        self.state.reset(self.backend.as_ref());
        Ok(())
    }

    fn suspend(&self) -> crate::DeviceManagerResult {
        self.state.suspend(self.backend.as_ref());
        Ok(())
    }

    fn resume(&self) -> crate::DeviceManagerResult {
        self.state.resume(alloc::sync::Arc::clone(&self.backend));
        Ok(())
    }
}

#[cfg(target_arch = "aarch64")]
impl Drop for VtimerLifecycle {
    fn drop(&mut self) {
        self.state.reset(self.backend.as_ref());
    }
}

#[cfg(target_arch = "aarch64")]
impl InterruptInjectionPort for VtimerInterruptInjectionPort {
    fn inject(&self, target: arm_vgic::vtimer::VtimerTarget, vector: u8) {
        self.backend.inject_virtual_interrupt(target, vector);
    }
}

#[cfg(target_arch = "aarch64")]
impl Aarch64VtimerFactory {
    /// Builds the three CNT* system-register device contributions.
    pub fn build(&self) -> crate::DeviceManagerResult<crate::DeviceBundle> {
        use alloc::sync::Arc;

        use arm_vgic::vtimer::{
            HostVtimerBackend, SysCntpCtlEl0, SysCntpTvalEl0, SysCntpctEl0, VtimerBackend,
            VtimerState,
        };
        use axdevice_base::SysRegDeviceAdapter;

        use crate::{DeviceBundle, DeviceRegistration};

        let backend: Arc<dyn VtimerBackend> = Arc::new(HostVtimerBackend);
        let interrupt_port: Arc<dyn InterruptInjectionPort> =
            Arc::new(VtimerInterruptInjectionPort {
                backend: Arc::clone(&backend),
            });
        let state = Arc::new(VtimerState::new());
        let lifecycle: Arc<dyn crate::DeviceLifecycle> = Arc::new(VtimerLifecycle {
            state: Arc::clone(&state),
            backend: Arc::clone(&backend),
        });

        let bundle = DeviceBundle::from_registration(DeviceRegistration::Device(Arc::new(
            SysRegDeviceAdapter::new(SysCntpCtlEl0::new(Arc::clone(&state), Arc::clone(&backend))),
        )))
        .with_registration(DeviceRegistration::Device(Arc::new(
            SysRegDeviceAdapter::new(SysCntpctEl0::new(Arc::clone(&backend))),
        )))
        .with_registration(DeviceRegistration::Device(Arc::new(
            SysRegDeviceAdapter::new(SysCntpTvalEl0::new(state, Arc::clone(&backend))),
        )));
        Ok(bundle
            .with_service::<Aarch64VtimerBackendKey>(backend)?
            .with_service::<InterruptInjectionPortKey>(interrupt_port)?
            .with_lifecycle(lifecycle))
    }
}
