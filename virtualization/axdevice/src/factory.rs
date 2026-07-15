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

//! Extensible construction of emulated devices from VM configuration.

use alloc::{sync::Arc, vec::Vec};

use axdevice_base::{IrqLine, MsiEndpoint};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{
    DeviceBundle, DeviceManagerError, DeviceManagerResult, InterruptTopology, MsiRequest,
    WiredIrqRequest,
};

/// VM-owned services available while a device factory is building a device.
pub struct DeviceBuildContext<'a> {
    interrupt_topology: &'a InterruptTopology,
}

impl<'a> DeviceBuildContext<'a> {
    /// Creates a device build context backed by one VM's interrupt topology.
    pub const fn new(interrupt_topology: &'a InterruptTopology) -> Self {
        Self { interrupt_topology }
    }

    /// Connects one device source to a wired interrupt-controller input.
    pub fn connect_irq(&self, request: WiredIrqRequest) -> DeviceManagerResult<IrqLine> {
        self.interrupt_topology.connect_irq(request)
    }

    /// Connects one device event to a message-signaled interrupt controller.
    pub fn connect_msi(&self, request: MsiRequest) -> DeviceManagerResult<MsiEndpoint> {
        self.interrupt_topology.connect_msi(request)
    }

    pub(crate) const fn interrupt_topology(&self) -> &InterruptTopology {
        self.interrupt_topology
    }
}

/// Builds all capabilities contributed by one emulated device type.
pub trait DeviceFactory: Send + Sync {
    /// Returns the configuration type handled by this factory.
    fn device_type(&self) -> EmulatedDeviceType;

    /// Builds a device without modifying the destination device registry.
    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle>;
}

/// A registry containing at most one factory for each emulated device type.
#[derive(Default)]
pub struct DeviceFactoryRegistry {
    factories: Vec<(EmulatedDeviceType, Arc<dyn DeviceFactory>)>,
}

impl DeviceFactoryRegistry {
    /// Creates an empty factory registry.
    pub const fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    /// Registers a factory, rejecting a duplicate device type.
    pub fn register(&mut self, factory: Arc<dyn DeviceFactory>) -> DeviceManagerResult {
        let device_type = factory.device_type();
        if self.get(device_type).is_some() {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "register device factory",
                detail: alloc::format!(
                    "factory for device type {device_type} is already registered"
                ),
            });
        }
        self.factories.push((device_type, factory));
        Ok(())
    }

    /// Returns the factory registered for `device_type`.
    pub fn get(&self, device_type: EmulatedDeviceType) -> Option<&dyn DeviceFactory> {
        self.factories
            .iter()
            .find(|(registered_type, _)| *registered_type == device_type)
            .map(|(_, factory)| factory.as_ref())
    }

    /// Builds a bundle for `config`.
    pub fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let Some(factory) = self.get(config.emu_type) else {
            return Err(DeviceManagerError::Unsupported {
                operation: "build emulated device",
                detail: alloc::format!(
                    "no factory is registered for emulated device '{}' of type {}",
                    config.name,
                    config.emu_type
                ),
            });
        };
        factory.build(config, context)
    }
}

struct MetaDeviceFactory;

impl DeviceFactory for MetaDeviceFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Dummy
    }

    fn build(
        &self,
        _config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        Ok(DeviceBundle::new())
    }
}

/// Registers device factories that do not depend on an architecture backend.
pub fn register_builtin_factories(registry: &mut DeviceFactoryRegistry) -> DeviceManagerResult {
    registry.register(Arc::new(MetaDeviceFactory))
}
