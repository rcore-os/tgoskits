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

use axdevice_base::{ControllerInputId, InterruptTriggerMode, IrqLine, VirtualInterruptController};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{
    DeviceBundle, DeviceManagerError, DeviceManagerResult, GuestRangeAllocatorKey,
    ServiceCardinality, ServiceKey, range_alloc::IvcGuestRangeAllocator,
};

/// Typed service key for the VM's canonical virtual interrupt controller.
pub struct VirtualInterruptControllerKey;

impl ServiceKey for VirtualInterruptControllerKey {
    type Service = dyn VirtualInterruptController;

    const NAME: &'static str = "virtual-interrupt-controller";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// VM-owned services available while a device factory is building a device.
pub struct DeviceBuildContext<'a> {
    interrupt_controller: &'a dyn VirtualInterruptController,
}

impl<'a> DeviceBuildContext<'a> {
    /// Creates a device build context backed by the VM's canonical controller.
    pub const fn new(interrupt_controller: &'a dyn VirtualInterruptController) -> Self {
        Self {
            interrupt_controller,
        }
    }

    /// Returns the VM's canonical virtual interrupt controller.
    pub const fn interrupt_controller(&self) -> &'a dyn VirtualInterruptController {
        self.interrupt_controller
    }

    /// Claims a source connection on one VM-local controller input.
    pub fn resolve_irq(
        &self,
        line: usize,
        trigger: InterruptTriggerMode,
    ) -> DeviceManagerResult<IrqLine> {
        Ok(self
            .interrupt_controller
            .wired_input(ControllerInputId::new(line), trigger)?
            .connect()?)
    }
}

/// Builds all capabilities contributed by one emulated device type.
///
/// A factory that exposes an architecture-owned, pre-created controller must
/// capture the same shared controller instance and validate that each build
/// request matches the configuration used to create it, including its MMIO
/// base, length, and type-specific arguments.
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
///
/// Registered factories are authoritative for their device type: during
/// [`DeviceRuntime::build_with_factories`](crate::DeviceRuntime::build_with_factories),
/// each configured device has exactly one construction path. A factory error
/// is propagated and never causes a fallback to create another device.
///
/// Architectures that pre-create an interrupt controller must first reject
/// duplicate controller configurations, then register exactly one factory that
/// captures the shared controller and its validated configuration fingerprint.
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

struct IvcChannelFactory;

impl DeviceFactory for IvcChannelFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::IVCChannel
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let allocator = IvcGuestRangeAllocator::new(config.base_gpa, config.length)?.into_service();
        DeviceBundle::new().with_service::<GuestRangeAllocatorKey>(allocator)
    }
}

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
    registry.register(Arc::new(MetaDeviceFactory))?;
    registry.register(Arc::new(IvcChannelFactory))
}
