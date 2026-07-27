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

use crate::{DeviceManagerResult, DeviceServices, ServiceKey};

/// A device capability that can be polled by the VM runtime.
pub trait PollableDeviceOps: Send + Sync {
    /// Advances the device using the current monotonic time in nanoseconds.
    fn poll(&self, now_ns: u64) -> DeviceManagerResult;
}

/// Optional lifecycle operations contributed by a device.
///
/// Lifecycle is deliberately separate from the hot-path [`Device`] trait:
/// registered devices are shared through [`Arc`], while lifecycle state must
/// remain internally synchronized by the capability that owns it.
pub trait DeviceLifecycle: Send + Sync {
    /// Restores the device to its power-on state.
    fn reset(&self) -> DeviceManagerResult;

    /// Quiesces the device before the VM is suspended.
    fn suspend(&self) -> DeviceManagerResult;

    /// Restores a suspended device.
    fn resume(&self) -> DeviceManagerResult;
}

/// One strongly typed capability contributed by a device.
#[non_exhaustive]
pub enum DeviceRegistration {
    /// A device implementing the unified [`Device`] trait.
    Device(Arc<dyn Device>),
    /// A capability that requires periodic polling.
    Pollable(Arc<dyn PollableDeviceOps>),
}

/// A set of device capabilities that must be registered atomically.
///
/// The contained registration lists are private so callers cannot bypass
/// [`DeviceRegistration`] when adding future capability kinds.
#[derive(Default)]
pub struct DeviceBundle {
    pub(crate) devices: Vec<Arc<dyn Device>>,
    /// Indices of devices that require access-scoped guest-memory capability.
    pub(crate) guest_memory_devices: Vec<usize>,
    pub(crate) pollable: Vec<Arc<dyn PollableDeviceOps>>,
    pub(crate) lifecycle: Vec<Arc<dyn DeviceLifecycle>>,
    pub(crate) services: DeviceServices,
}

impl DeviceBundle {
    /// Creates an empty bundle.
    pub const fn new() -> Self {
        Self {
            devices: Vec::new(),
            guest_memory_devices: Vec::new(),
            pollable: Vec::new(),
            lifecycle: Vec::new(),
            services: DeviceServices::new(),
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
        }
    }

    /// Adds a device that requires guest-memory access during a routed access.
    ///
    /// This is a declaration, not a memory handle: the runtime assigns the
    /// final [`axdevice_base::DeviceId`] during registration and injects the
    /// actual port only for the duration of one eligible bus access.
    pub fn add_guest_memory_device(&mut self, device: Arc<dyn Device>) {
        self.guest_memory_devices.push(self.devices.len());
        self.devices.push(device);
    }

    /// Adds a guest-memory-capable device and returns the bundle.
    pub fn with_guest_memory_device(mut self, device: Arc<dyn Device>) -> Self {
        self.add_guest_memory_device(device);
        self
    }

    /// Adds one capability and returns the bundle for builder-style use.
    pub fn with_registration(mut self, registration: DeviceRegistration) -> Self {
        self.push(registration);
        self
    }

    /// Adds one lifecycle capability to this contribution.
    pub fn add_lifecycle(&mut self, lifecycle: Arc<dyn DeviceLifecycle>) {
        self.lifecycle.push(lifecycle);
    }

    /// Adds one lifecycle capability and returns the contribution.
    pub fn with_lifecycle(mut self, lifecycle: Arc<dyn DeviceLifecycle>) -> Self {
        self.add_lifecycle(lifecycle);
        self
    }

    /// Adds one typed service provider to this contribution.
    ///
    /// # Errors
    ///
    /// Returns an error if this contribution already provides a
    /// single-provider service with the same key.
    pub fn provide_service<K: ServiceKey>(
        &mut self,
        service: Arc<K::Service>,
    ) -> DeviceManagerResult {
        self.services.provide::<K>(service)
    }

    /// Adds one typed service provider and returns the contribution.
    ///
    /// # Errors
    ///
    /// Returns an error if the contribution already provides a
    /// single-provider service with the same key.
    pub fn with_service<K: ServiceKey>(
        mut self,
        service: Arc<K::Service>,
    ) -> DeviceManagerResult<Self> {
        self.provide_service::<K>(service)?;
        Ok(self)
    }

    /// Returns whether this bundle contains no capabilities.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
            && self.pollable.is_empty()
            && self.lifecycle.is_empty()
            && self.services.is_empty()
    }
}

impl From<DeviceRegistration> for DeviceBundle {
    fn from(registration: DeviceRegistration) -> Self {
        Self::from_registration(registration)
    }
}
