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

//! Factory discovery and VM-owned construction context.

use alloc::{sync::Arc, vec::Vec};

use axdevice_base::{DeviceBundle, InterruptTriggerMode, IrqLine, IrqResult};
pub use axdevice_base::{
    DeviceFactory, DeviceFactoryContext, DeviceFactoryError, DeviceFactoryRegister,
    DeviceFactoryResult,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{DeviceManagerError, DeviceManagerResult};

/// Resolves a VM-local interrupt line for a device under construction.
pub trait IrqResolver: Send + Sync {
    /// Resolves `line` with the requested trigger mode.
    fn resolve_irq(&self, line: usize, trigger: InterruptTriggerMode) -> IrqResult<IrqLine>;
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
    pub fn resolve_irq(&self, line: usize, trigger: InterruptTriggerMode) -> IrqResult<IrqLine> {
        self.irq_resolver.resolve_irq(line, trigger)
    }
}

impl DeviceFactoryContext for DeviceBuildContext<'_> {
    fn resolve_irq(
        &self,
        line: usize,
        trigger: InterruptTriggerMode,
    ) -> DeviceFactoryResult<IrqLine> {
        self.irq_resolver
            .resolve_irq(line, trigger)
            .map_err(DeviceFactoryError::from)
    }
}

enum RegisteredFactory {
    Static(&'static dyn DeviceFactory),
    Owned(Arc<dyn DeviceFactory>),
}

impl RegisteredFactory {
    fn factory(&self) -> &dyn DeviceFactory {
        match self {
            Self::Static(factory) => *factory,
            Self::Owned(factory) => factory.as_ref(),
        }
    }
}

/// Runtime catalog of factories discovered from the final image.
///
/// The default VM initialization path populates this catalog from linker
/// registrations. [`Self::register`] remains available for tests and temporary
/// architecture adapters while their factories migrate to static registration.
#[derive(Default)]
pub struct DeviceFactoryRegistry {
    factories: Vec<(EmulatedDeviceType, RegisteredFactory)>,
}

impl DeviceFactoryRegistry {
    /// Creates an empty factory catalog.
    pub const fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    /// Registers an explicitly provided factory, rejecting a duplicate type.
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
        self.factories
            .push((device_type, RegisteredFactory::Owned(factory)));
        Ok(())
    }

    /// Adds statically linked factory registrations to this catalog.
    ///
    /// The full set is validated before insertion, so duplicate types leave the
    /// catalog unchanged.
    pub fn register_static_factories(
        &mut self,
        registers: &[DeviceFactoryRegister],
    ) -> DeviceManagerResult {
        for (index, register) in registers.iter().enumerate() {
            let device_type = register.factory().device_type();
            if self.get(device_type).is_some() {
                return Err(DeviceManagerError::ResourceConflict {
                    operation: "register static device factory",
                    detail: alloc::format!(
                        "factory '{}' duplicates an existing factory for device type {device_type}",
                        register.name()
                    ),
                });
            }
            if let Some(existing) = registers[..index]
                .iter()
                .find(|existing| existing.factory().device_type() == device_type)
            {
                return Err(DeviceManagerError::ResourceConflict {
                    operation: "register static device factory",
                    detail: alloc::format!(
                        "factories '{}' and '{}' both handle device type {device_type}",
                        existing.name(),
                        register.name()
                    ),
                });
            }
        }

        for register in registers {
            self.factories.push((
                register.factory().device_type(),
                RegisteredFactory::Static(register.factory()),
            ));
        }
        Ok(())
    }

    /// Returns the factory registered for `device_type`.
    pub fn get(&self, device_type: EmulatedDeviceType) -> Option<&dyn DeviceFactory> {
        self.factories
            .iter()
            .find(|(registered_type, _)| *registered_type == device_type)
            .map(|(_, factory)| factory.factory())
    }

    /// Builds a device bundle for one VM configuration entry.
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
        factory.build(config, context).map_err(Into::into)
    }
}

/// Returns factory registrations collected from the final linker image.
#[cfg(any(target_os = "none", target_env = "musl"))]
pub fn linker_device_factory_registers() -> DeviceManagerResult<&'static [DeviceFactoryRegister]> {
    unsafe extern "C" {
        fn __saxdevice_factory();
        fn __eaxdevice_factory();
    }

    let start = __saxdevice_factory as *const () as usize;
    let end = __eaxdevice_factory as *const () as usize;
    if start > end {
        return Err(DeviceManagerError::InvalidConfig {
            operation: "read device factory linker section",
            detail: alloc::format!("section start {start:#x} is after section end {end:#x}"),
        });
    }

    let byte_len = end - start;
    if byte_len == 0 {
        return Ok(&[]);
    }

    let alignment = core::mem::align_of::<DeviceFactoryRegister>();
    if !start.is_multiple_of(alignment) {
        return Err(DeviceManagerError::InvalidConfig {
            operation: "read device factory linker section",
            detail: alloc::format!("section start {start:#x} is not aligned to {alignment} bytes"),
        });
    }

    let entry_size = core::mem::size_of::<DeviceFactoryRegister>();
    if !byte_len.is_multiple_of(entry_size) {
        return Err(DeviceManagerError::InvalidConfig {
            operation: "read device factory linker section",
            detail: alloc::format!(
                "section length {byte_len:#x} is not a multiple of entry size {entry_size}"
            ),
        });
    }

    let entry_count = byte_len / entry_size;
    // SAFETY: `runtime.ld` places only `DeviceFactoryRegister` values between
    // these symbols, keeps the input sections, and aligns their start. The
    // checks above validate the remaining range and layout preconditions.
    Ok(unsafe { core::slice::from_raw_parts(start as *const DeviceFactoryRegister, entry_count) })
}

/// Returns no linker registrations on targets without the runtime linker script.
#[cfg(not(any(target_os = "none", target_env = "musl")))]
pub fn linker_device_factory_registers() -> DeviceManagerResult<&'static [DeviceFactoryRegister]> {
    Ok(&[])
}

/// Registers every factory collected from the final linker image.
pub fn register_linker_factories(registry: &mut DeviceFactoryRegistry) -> DeviceManagerResult {
    registry.register_static_factories(linker_device_factory_registers()?)
}

struct MetaDeviceFactory;

static META_DEVICE_FACTORY: MetaDeviceFactory = MetaDeviceFactory;

axdevice_base::register_device_factory!("meta", META_DEVICE_FACTORY);

impl DeviceFactory for MetaDeviceFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Dummy
    }

    fn build(
        &self,
        _config: &EmulatedDeviceConfig,
        _context: &dyn DeviceFactoryContext,
    ) -> DeviceFactoryResult<DeviceBundle> {
        Ok(DeviceBundle::new())
    }
}

/// Registers built-in factories on targets without the runtime linker script.
#[cfg(not(any(target_os = "none", target_env = "musl")))]
pub fn register_builtin_factories(registry: &mut DeviceFactoryRegistry) -> DeviceManagerResult {
    static META_REGISTER: DeviceFactoryRegister =
        DeviceFactoryRegister::new("meta", &META_DEVICE_FACTORY);
    registry.register_static_factories(core::slice::from_ref(&META_REGISTER))
}

/// ArceOS image built-ins are discovered from `.axdevice.factory`.
#[cfg(any(target_os = "none", target_env = "musl"))]
pub fn register_builtin_factories(_registry: &mut DeviceFactoryRegistry) -> DeviceManagerResult {
    Ok(())
}
