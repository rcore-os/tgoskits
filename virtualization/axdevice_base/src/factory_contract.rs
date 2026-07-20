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

//! Device factory capabilities shared by device implementations and VM containers.

use alloc::{string::String, sync::Arc, vec::Vec};

use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, InterruptTriggerMode};

use crate::{Device, DeviceError, DeviceResult, IrqError, IrqLine};

/// Errors reported while a factory validates configuration or builds devices.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DeviceFactoryError {
    /// The VM device configuration is malformed or inconsistent.
    #[error("invalid device factory configuration for {operation}: {detail}")]
    InvalidConfig {
        /// The factory operation that rejected the configuration.
        operation: &'static str,
        /// Diagnostic detail describing the invalid configuration.
        detail: String,
    },
    /// A constructed device or device adapter reported an error.
    #[error(transparent)]
    Device(#[from] DeviceError),
    /// Resolving a VM-local interrupt line failed.
    #[error(transparent)]
    Irq(#[from] IrqError),
}

/// Result type returned by device factory operations.
pub type DeviceFactoryResult<T = ()> = Result<T, DeviceFactoryError>;

/// VM-owned services available while a device factory builds a device.
pub trait DeviceFactoryContext: Send + Sync {
    /// Resolves a VM-local interrupt line with the requested trigger mode.
    fn resolve_irq(
        &self,
        line: usize,
        trigger: InterruptTriggerMode,
    ) -> DeviceFactoryResult<IrqLine>;
}

/// A device capability that can be polled by the VM runtime.
pub trait PollableDeviceOps: Send + Sync {
    /// Advances the device using the current monotonic time in nanoseconds.
    fn poll(&self, now_ns: u64) -> DeviceResult;
}

/// One strongly typed capability contributed by a device factory.
#[non_exhaustive]
pub enum DeviceRegistration {
    /// A device implementing the unified [`Device`] trait.
    Device(Arc<dyn Device>),
    /// A capability that requires periodic polling.
    Pollable(Arc<dyn PollableDeviceOps>),
}

/// Device capability lists consumed by a VM device container.
pub struct DeviceBundleParts {
    /// Devices that participate in bus routing and resource registration.
    pub devices: Vec<Arc<dyn Device>>,
    /// Device capabilities that require periodic polling.
    pub pollable_devices: Vec<Arc<dyn PollableDeviceOps>>,
}

/// A set of device capabilities built from one VM device configuration.
#[derive(Default)]
pub struct DeviceBundle {
    devices: Vec<Arc<dyn Device>>,
    pollable: Vec<Arc<dyn PollableDeviceOps>>,
}

impl DeviceBundle {
    /// Creates an empty bundle.
    pub const fn new() -> Self {
        Self {
            devices: Vec::new(),
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
            DeviceRegistration::Device(device) => self.devices.push(device),
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
        self.devices.is_empty() && self.pollable.is_empty()
    }

    /// Splits the bundle into the capabilities consumed by the VM device container.
    pub fn into_parts(self) -> DeviceBundleParts {
        DeviceBundleParts {
            devices: self.devices,
            pollable_devices: self.pollable,
        }
    }
}

impl From<DeviceRegistration> for DeviceBundle {
    fn from(registration: DeviceRegistration) -> Self {
        Self::from_registration(registration)
    }
}

/// Builds all capabilities contributed by one emulated device type.
pub trait DeviceFactory: Send + Sync {
    /// Returns the VM configuration type handled by this factory.
    fn device_type(&self) -> EmulatedDeviceType;

    /// Builds device instances from one VM device configuration.
    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &dyn DeviceFactoryContext,
    ) -> DeviceFactoryResult<DeviceBundle>;
}

/// Static factory registration entry collected from the final image.
///
/// This is an internal Rust-image layout contract, not a cross-language ABI.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DeviceFactoryRegister {
    name: &'static str,
    factory: &'static dyn DeviceFactory,
}

impl DeviceFactoryRegister {
    /// Creates a static factory registration entry.
    pub const fn new(name: &'static str, factory: &'static dyn DeviceFactory) -> Self {
        Self { name, factory }
    }

    /// Returns the human-readable factory name used for diagnostics.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the registered factory.
    pub const fn factory(&self) -> &'static dyn DeviceFactory {
        self.factory
    }
}

/// Registers a device factory in the `.axdevice.factory` linker section.
///
/// The concrete device crate must be linked into the final image. The final
/// linker script must retain `.axdevice.factory*` and export the section's
/// start and end symbols.
#[macro_export]
macro_rules! register_device_factory {
    ($name:expr, $factory:expr $(,)?) => {
        const _: () = {
            #[unsafe(link_section = ".axdevice.factory")]
            #[used]
            static FACTORY_REGISTER: $crate::DeviceFactoryRegister =
                $crate::DeviceFactoryRegister::new($name, &$factory);
        };
    };
}
