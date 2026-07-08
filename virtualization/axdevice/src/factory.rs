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

use ax_errno::{AxResult, ax_err};
use axdevice_base::{InterruptTriggerMode, IrqLine};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::DeviceBundle;

/// Resolves a VM-local interrupt line for a device under construction.
pub trait IrqResolver: Send + Sync {
    /// Resolves `line` with the requested trigger mode.
    fn resolve_irq(&self, line: usize, trigger: InterruptTriggerMode) -> AxResult<IrqLine>;
}

/// VM-owned services available while a device factory is building a device.
pub struct DeviceBuildContext<'a> {
    irq_resolver: &'a dyn IrqResolver,
}

impl<'a> DeviceBuildContext<'a> {
    /// Creates a device build context backed by `irq_resolver`.
    pub const fn new(irq_resolver: &'a dyn IrqResolver) -> Self {
        Self { irq_resolver }
    }

    /// Resolves a VM-local interrupt line.
    pub fn resolve_irq(&self, line: usize, trigger: InterruptTriggerMode) -> AxResult<IrqLine> {
        self.irq_resolver.resolve_irq(line, trigger)
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
    ) -> AxResult<DeviceBundle>;
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
    pub fn register(&mut self, factory: Arc<dyn DeviceFactory>) -> AxResult {
        let device_type = factory.device_type();
        if self.get(device_type).is_some() {
            return ax_err!(
                AlreadyExists,
                format_args!("factory for device type {device_type} is already registered")
            );
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
    ) -> AxResult<DeviceBundle> {
        let Some(factory) = self.get(config.emu_type) else {
            return ax_err!(
                Unsupported,
                format_args!(
                    "no factory is registered for emulated device '{}' of type {}",
                    config.name, config.emu_type
                )
            );
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
    ) -> AxResult<DeviceBundle> {
        Ok(DeviceBundle::new())
    }
}

/// Registers device factories that do not depend on an architecture backend.
pub fn register_builtin_factories(registry: &mut DeviceFactoryRegistry) -> AxResult {
    registry.register(Arc::new(MetaDeviceFactory))
}
