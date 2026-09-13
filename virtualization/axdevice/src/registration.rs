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

use axdevice_base::*;

use crate::{interrupt::*, *};

/// A device capability that can be polled by the VM runtime.
pub trait PollableDeviceOps: Send + Sync {
    /// Advances the device using the current monotonic time in nanoseconds.
    fn poll(&self, now_ns: u64) -> DeviceManagerResult;
}

/// A device capability that advances asynchronous DMA work with scoped guest
/// memory access.
///
/// The runtime supplies the access port only for this call. Implementations
/// must not retain it after [`poll_dma`](Self::poll_dma) returns.
pub trait DmaPollableDeviceOps: Send + Sync {
    /// Advances pending DMA work using the current monotonic time.
    fn poll_dma(
        &self,
        now_ns: u64,
        context: &mut dyn DeviceContext,
        grant: &DmaGrant,
    ) -> DeviceManagerResult;
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
    /// A VM-local virtual interrupt controller capability.
    InterruptController(ControllerRegistration),
}

/// A set of device capabilities that must be registered atomically.
///
/// The contained registration lists are private so callers cannot bypass
/// [`DeviceRegistration`] when adding future capability kinds.
#[derive(Default)]
pub struct DeviceBundle {
    pub(crate) devices: Vec<Arc<dyn Device>>,
    /// Indices and tokens of devices that require access-scoped guest-memory capability.
    pub(crate) guest_memory_devices: Vec<(usize, DmaGrant)>,
    /// Indices and tokens of devices that require timer scheduling capability.
    pub(crate) timer_devices: Vec<(usize, TimerGrant)>,
    /// Indices and tokens of devices that require vCPU wake capability.
    pub(crate) wake_devices: Vec<(usize, WakeGrant)>,
    /// Indices and tokens of devices that require VM stop-request capability.
    pub(crate) stop_devices: Vec<(usize, StopGrant)>,
    pub(crate) pollable: Vec<Arc<dyn PollableDeviceOps>>,
    /// DMA pollers paired with their bundle-local device and grant.
    pub(crate) dma_pollable: Vec<(usize, Arc<dyn DmaPollableDeviceOps>, DmaGrant)>,
    pub(crate) lifecycle: Vec<Arc<dyn DeviceLifecycle>>,
    pub(crate) services: DeviceServices,
    pub(crate) planned: PlannedBundleResources,
    pub(crate) pci_function: Option<BundlePciFunction>,
}

pub(crate) struct BundlePciFunction {
    pub(crate) device_index: usize,
    pub(crate) function: Arc<dyn PciFunction>,
}

impl DeviceBundle {
    /// Creates an empty bundle.
    pub const fn new() -> Self {
        Self {
            devices: Vec::new(),
            guest_memory_devices: Vec::new(),
            timer_devices: Vec::new(),
            wake_devices: Vec::new(),
            stop_devices: Vec::new(),
            pollable: Vec::new(),
            dma_pollable: Vec::new(),
            lifecycle: Vec::new(),
            services: DeviceServices::new(),
            planned: PlannedBundleResources::new(),
            pci_function: None,
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
                self.planned.controllers.push(controller);
            }
        }
    }

    /// Adds one unified device and returns its bundle-local index.
    pub fn add_device(&mut self, device: Arc<dyn Device>) -> usize {
        let index = self.devices.len();
        self.devices.push(device);
        index
    }

    /// Adds one device as this graph node's resolved PCI function.
    pub fn add_pci_function(
        &mut self,
        function: Arc<dyn PciFunction>,
    ) -> DeviceManagerResult<usize> {
        if self.pci_function.is_some() {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "declare bundled PCI function",
                detail: "a device bundle may bind at most one PCI function".into(),
            });
        }
        let device: Arc<dyn Device> = function.clone();
        let device_index = self.add_device(device);
        self.pci_function = Some(BundlePciFunction {
            device_index,
            function,
        });
        Ok(device_index)
    }

    /// Grants guest-memory access to an already-added bundle-local device.
    pub fn grant_guest_memory_to_device(&mut self, device_index: usize, grant: DmaGrant) {
        self.guest_memory_devices.push((device_index, grant));
    }

    /// Grants timer scheduling to an already-added bundle-local device.
    pub fn grant_timer_to_device(&mut self, device_index: usize, grant: TimerGrant) {
        self.timer_devices.push((device_index, grant));
    }

    /// Grants vCPU wake access to an already-added bundle-local device.
    pub fn grant_wake_to_device(&mut self, device_index: usize, grant: WakeGrant) {
        self.wake_devices.push((device_index, grant));
    }

    /// Grants VM stop-request access to an already-added bundle-local device.
    pub fn grant_stop_to_device(&mut self, device_index: usize, grant: StopGrant) {
        self.stop_devices.push((device_index, grant));
    }

    /// Adds a device that requires guest-memory access during a routed access.
    ///
    /// This is a declaration, not a memory handle: the runtime assigns the
    /// final [`axdevice_base::DeviceId`] during registration and injects the
    /// actual port only for the duration of one eligible bus access.
    pub fn add_guest_memory_device(&mut self, device: Arc<dyn Device>) {
        let device_index = self.add_device(device);
        self.grant_guest_memory_to_device(device_index, DmaGrant::new());
    }

    /// Adds a device with an explicit guest-memory grant token.
    pub fn add_guest_memory_device_with_grant(&mut self, device: Arc<dyn Device>, grant: DmaGrant) {
        let device_index = self.add_device(device);
        self.grant_guest_memory_to_device(device_index, grant);
    }

    /// Adds one device whose asynchronous progress requires scoped guest
    /// memory access.
    pub fn add_dma_pollable_device(
        &mut self,
        device: Arc<dyn Device>,
        pollable: Arc<dyn DmaPollableDeviceOps>,
        grant: DmaGrant,
    ) {
        let device_index = self.add_device(device);
        self.grant_guest_memory_to_device(device_index, grant.clone());
        self.dma_pollable.push((device_index, pollable, grant));
    }

    /// Adds a timer-capable device with an explicit grant token.
    pub fn add_timer_device_with_grant(&mut self, device: Arc<dyn Device>, grant: TimerGrant) {
        let device_index = self.add_device(device);
        self.grant_timer_to_device(device_index, grant);
    }

    /// Adds a vCPU-wake-capable device with an explicit grant token.
    pub fn add_wake_device_with_grant(&mut self, device: Arc<dyn Device>, grant: WakeGrant) {
        let device_index = self.add_device(device);
        self.grant_wake_to_device(device_index, grant);
    }

    /// Adds a VM-stop-capable device with an explicit grant token.
    pub fn add_stop_device_with_grant(&mut self, device: Arc<dyn Device>, grant: StopGrant) {
        let device_index = self.add_device(device);
        self.grant_stop_to_device(device_index, grant);
    }

    /// Adds a guest-memory-capable device with an explicit grant token.
    pub fn with_guest_memory_device_grant(
        mut self,
        device: Arc<dyn Device>,
        grant: DmaGrant,
    ) -> Self {
        self.add_guest_memory_device_with_grant(device, grant);
        self
    }

    /// Adds a timer-capable device with an explicit grant token.
    pub fn with_timer_device_grant(mut self, device: Arc<dyn Device>, grant: TimerGrant) -> Self {
        self.add_timer_device_with_grant(device, grant);
        self
    }

    /// Adds a vCPU-wake-capable device with an explicit grant token.
    pub fn with_wake_device_grant(mut self, device: Arc<dyn Device>, grant: WakeGrant) -> Self {
        self.add_wake_device_with_grant(device, grant);
        self
    }

    /// Adds a VM-stop-capable device with an explicit grant token.
    pub fn with_stop_device_grant(mut self, device: Arc<dyn Device>, grant: StopGrant) -> Self {
        self.add_stop_device_with_grant(device, grant);
        self
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
            && self.guest_memory_devices.is_empty()
            && self.timer_devices.is_empty()
            && self.wake_devices.is_empty()
            && self.stop_devices.is_empty()
            && self.pollable.is_empty()
            && self.dma_pollable.is_empty()
            && self.lifecycle.is_empty()
            && self.services.is_empty()
            && self.planned.is_empty()
    }
}

impl From<DeviceRegistration> for DeviceBundle {
    fn from(registration: DeviceRegistration) -> Self {
        Self::from_registration(registration)
    }
}
