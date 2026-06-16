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

//! Transactional device registration types.

use alloc::{sync::Arc, vec::Vec};

use ax_errno::AxResult;
use axdevice_base::{BaseMmioDeviceOps, BasePortDeviceOps, BaseSysRegDeviceOps};

/// A device capability that can be polled by the VM runtime.
pub trait PollableDeviceOps: Send + Sync {
    /// Advances the device using the current monotonic time in nanoseconds.
    fn poll(&self, now_ns: u64) -> AxResult;
}

/// One strongly typed capability contributed by a device.
#[non_exhaustive]
pub enum DeviceRegistration {
    /// An MMIO access capability.
    Mmio(Arc<dyn BaseMmioDeviceOps>),
    /// A port I/O access capability.
    Port(Arc<dyn BasePortDeviceOps>),
    /// A system register access capability.
    SysReg(Arc<dyn BaseSysRegDeviceOps>),
    /// A capability that requires periodic polling.
    Pollable(Arc<dyn PollableDeviceOps>),
}

/// A set of device capabilities that must be registered atomically.
///
/// The contained registration lists are private so callers cannot bypass
/// [`DeviceRegistration`] when adding future capability kinds.
#[derive(Default)]
pub struct DeviceBundle {
    pub(crate) mmio: Vec<Arc<dyn BaseMmioDeviceOps>>,
    pub(crate) port: Vec<Arc<dyn BasePortDeviceOps>>,
    pub(crate) sysreg: Vec<Arc<dyn BaseSysRegDeviceOps>>,
    pub(crate) pollable: Vec<Arc<dyn PollableDeviceOps>>,
}

impl DeviceBundle {
    /// Creates an empty bundle.
    pub const fn new() -> Self {
        Self {
            mmio: Vec::new(),
            port: Vec::new(),
            sysreg: Vec::new(),
            pollable: Vec::new(),
        }
    }

    /// Creates a bundle containing one registration.
    pub fn from_registration(registration: DeviceRegistration) -> Self {
        let mut bundle = Self::new();
        bundle.push(registration);
        bundle
    }

    /// Adds one capability to this bundle.
    pub fn push(&mut self, registration: DeviceRegistration) {
        match registration {
            DeviceRegistration::Mmio(device) => self.mmio.push(device),
            DeviceRegistration::Port(device) => self.port.push(device),
            DeviceRegistration::SysReg(device) => self.sysreg.push(device),
            DeviceRegistration::Pollable(device) => self.pollable.push(device),
        }
    }

    /// Adds one capability and returns the bundle for builder-style use.
    pub fn with_registration(mut self, registration: DeviceRegistration) -> Self {
        self.push(registration);
        self
    }

    /// Returns whether this bundle contains no capabilities.
    pub fn is_empty(&self) -> bool {
        self.mmio.is_empty()
            && self.port.is_empty()
            && self.sysreg.is_empty()
            && self.pollable.is_empty()
    }
}

impl From<DeviceRegistration> for DeviceBundle {
    fn from(registration: DeviceRegistration) -> Self {
        Self::from_registration(registration)
    }
}
