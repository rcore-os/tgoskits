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

use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{
    DeviceBuildContext, DeviceBundle, DeviceDeclaration, DeviceManagerError, DeviceManagerResult,
    DeviceRequirements, GuestRangeAllocatorKey, ResourceRequest, ResourceSlot,
    range_alloc::IvcGuestRangeAllocator,
};

/// Builds all capabilities contributed by one emulated device type.
///
/// A factory that exposes an architecture-owned, pre-created controller must
/// capture the same shared controller instance and validate that each build
/// request matches the configuration used to create it, including its MMIO
/// base, length, and type-specific arguments.
pub trait DeviceFactory: Send + Sync {
    /// Returns the configuration type handled by this factory.
    fn device_type(&self) -> EmulatedDeviceType;

    /// Validates immutable configuration and declares named resources.
    fn declare(&self, config: &EmulatedDeviceConfig) -> DeviceManagerResult<DeviceDeclaration>;

    /// Builds a device without modifying the destination device registry.
    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
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

    /// Clones the factory registered for `device_type`.
    pub fn get_arc(&self, device_type: EmulatedDeviceType) -> Option<Arc<dyn DeviceFactory>> {
        self.factories
            .iter()
            .find(|(registered_type, _)| *registered_type == device_type)
            .map(|(_, factory)| factory.clone())
    }

    /// Builds a bundle for `config`.
    pub fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
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

    fn declare(&self, config: &EmulatedDeviceConfig) -> DeviceManagerResult<DeviceDeclaration> {
        let base = u64::try_from(config.base_gpa).map_err(ivc_range_conversion_error)?;
        let size = u64::try_from(config.length).map_err(ivc_range_conversion_error)?;
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new("guest-window")?,
                size,
                0x1000,
                ResourceRequest::Fixed(base),
            )
            .map(DeviceDeclaration::with_requirements)
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let (base, size) = if context.uses_planned_resources() {
            context.mmio(&ResourceSlot::new("guest-window")?)?
        } else {
            (
                u64::try_from(config.base_gpa).map_err(ivc_range_conversion_error)?,
                u64::try_from(config.length).map_err(ivc_range_conversion_error)?,
            )
        };
        let base = usize::try_from(base).map_err(ivc_range_conversion_error)?;
        let size = usize::try_from(size).map_err(ivc_range_conversion_error)?;
        let allocator = IvcGuestRangeAllocator::new(base, size)?.into_service();
        DeviceBundle::new().with_service::<GuestRangeAllocatorKey>(allocator)
    }
}

fn ivc_range_conversion_error(_error: core::num::TryFromIntError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "declare IVC guest resource window",
        detail: "IVC guest window does not fit the planner address width".into(),
    }
}

impl DeviceFactory for MetaDeviceFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Dummy
    }

    fn declare(&self, _config: &EmulatedDeviceConfig) -> DeviceManagerResult<DeviceDeclaration> {
        Ok(DeviceDeclaration::new())
    }

    fn build(
        &self,
        _config: &EmulatedDeviceConfig,
        _context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        Ok(DeviceBundle::new())
    }
}

/// Registers device factories that do not depend on an architecture backend.
pub fn register_builtin_factories(registry: &mut DeviceFactoryRegistry) -> DeviceManagerResult {
    registry.register(Arc::new(MetaDeviceFactory))?;
    registry.register(Arc::new(IvcChannelFactory))
}
