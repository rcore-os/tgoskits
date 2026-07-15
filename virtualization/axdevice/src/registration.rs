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

use axdevice_base::Device;

use crate::{ControllerRegistration, DeviceManagerResult};

/// A device capability that can be polled by the VM runtime.
pub trait PollableDeviceOps: Send + Sync {
    /// Advances the device using the current monotonic time in nanoseconds.
    fn poll(&self, now_ns: u64) -> DeviceManagerResult;
}

/// One strongly typed capability contributed by a device.
#[non_exhaustive]
pub enum DeviceRegistration {
    /// A device implementing the unified [`Device`] trait.
    Device(Arc<dyn Device>),
    /// A capability that requires periodic polling.
    Pollable(Arc<dyn PollableDeviceOps>),
    /// An interrupt controller and its connection capabilities.
    InterruptController(ControllerRegistration),
}

/// A set of device capabilities that must be registered atomically.
///
/// The contained registration lists are private so callers cannot bypass
/// [`DeviceRegistration`] when adding future capability kinds.
#[derive(Default)]
pub struct DeviceBundle {
    pub(crate) devices: Vec<Arc<dyn Device>>,
    pub(crate) pollable: Vec<Arc<dyn PollableDeviceOps>>,
    pub(crate) interrupt_controllers: Vec<ControllerRegistration>,
}

impl DeviceBundle {
    /// Creates an empty bundle.
    pub const fn new() -> Self {
        Self {
            devices: Vec::new(),
            pollable: Vec::new(),
            interrupt_controllers: Vec::new(),
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
            DeviceRegistration::Device(device) => self.devices.push(device),
            DeviceRegistration::Pollable(device) => self.pollable.push(device),
            DeviceRegistration::InterruptController(controller) => {
                self.interrupt_controllers.push(controller);
            }
        }
    }

    /// Adds one capability and returns the bundle for builder-style use.
    pub fn with_registration(mut self, registration: DeviceRegistration) -> Self {
        self.push(registration);
        self
    }

    /// Returns whether this bundle contains no capabilities.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty() && self.pollable.is_empty() && self.interrupt_controllers.is_empty()
    }
}

impl From<DeviceRegistration> for DeviceBundle {
    fn from(registration: DeviceRegistration) -> Self {
        Self::from_registration(registration)
    }
}
