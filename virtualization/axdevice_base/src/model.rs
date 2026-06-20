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

//! Core emulated device model traits and errors.

use alloc::{rc::Rc, string::String, vec::Vec};
use core::{any::Any, fmt};

use ax_errno::AxError;
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{
    AccessWidth, BusAccess, BusAddress, BusKind, BusResponse, DeviceCapabilities, Resource,
};

/// Unique identifier for a device instance inside one registry.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(usize);

impl DeviceId {
    /// Creates a device identifier from a raw numeric value.
    pub const fn new(id: usize) -> Self {
        Self(id)
    }

    /// Returns the raw numeric identifier.
    pub const fn raw(self) -> usize {
        self.0
    }
}

/// Common registry-facing metadata stored by native device implementations.
#[derive(Debug, Clone)]
pub struct DeviceMeta {
    id: DeviceId,
    name: String,
    resources: Vec<Resource>,
    capabilities: DeviceCapabilities,
}

impl DeviceMeta {
    /// Creates device metadata from raw parts.
    pub fn new(
        id: DeviceId,
        name: String,
        resources: Vec<Resource>,
        capabilities: DeviceCapabilities,
    ) -> Self {
        Self {
            id,
            name,
            resources,
            capabilities,
        }
    }

    /// Returns the registry-local device identifier.
    pub const fn id(&self) -> DeviceId {
        self.id
    }

    /// Returns the human-readable device instance name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns resources declared by this device.
    pub fn resources(&self) -> &[Resource] {
        &self.resources
    }

    /// Returns device capability flags.
    pub const fn capabilities(&self) -> DeviceCapabilities {
        self.capabilities
    }
}

/// Error type used by the new device abstraction layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceError {
    /// No device was registered for the requested bus access.
    DeviceNotFound {
        /// Bus namespace searched by the router.
        kind: BusKind,
        /// Address that missed all registered device resources.
        address: BusAddress,
    },
    /// A device with the same registry-local identifier already exists.
    DuplicateDeviceId {
        /// Duplicated identifier.
        id: DeviceId,
    },
    /// The bus kind and concrete bus address do not describe the same namespace.
    BusAddressMismatch {
        /// Expected or requested bus namespace.
        kind: BusKind,
        /// Address carrying a different bus namespace.
        address: BusAddress,
    },
    /// A resource conflicts with an already registered resource.
    ResourceConflict {
        /// Existing registered resource.
        existing: Resource,
        /// Newly requested resource.
        requested: Resource,
    },
    /// The access width is not accepted by the device or bus.
    InvalidAccessWidth {
        /// Rejected access width.
        width: AccessWidth,
    },
    /// The address does not belong to the resource handled by the selected device.
    AddressOutOfRange {
        /// Bus namespace used by the access.
        kind: BusKind,
        /// Rejected address.
        address: BusAddress,
    },
    /// A write was attempted on a read-only register.
    ReadOnly {
        /// Bus namespace used by the access.
        kind: BusKind,
        /// Rejected address.
        address: BusAddress,
    },
    /// A read was attempted on a write-only register.
    WriteOnly {
        /// Bus namespace used by the access.
        kind: BusKind,
        /// Rejected address.
        address: BusAddress,
    },
    /// The operation is validly formed but unsupported by this device/backend.
    UnsupportedOperation,
    /// Error returned by an existing backend while it is adapted into the new model.
    Backend(AxError),
}

impl From<AxError> for DeviceError {
    fn from(error: AxError) -> Self {
        Self::Backend(error)
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceNotFound { kind, address } => {
                write!(f, "device not found for {kind:?} address {address:?}")
            }
            Self::DuplicateDeviceId { id } => {
                write!(f, "duplicate device id {:?}", id)
            }
            Self::BusAddressMismatch { kind, address } => {
                write!(f, "bus kind {kind:?} does not match address {address:?}")
            }
            Self::ResourceConflict {
                existing,
                requested,
            } => write!(
                f,
                "device resource conflict: existing {existing:?}, requested {requested:?}"
            ),
            Self::InvalidAccessWidth { width } => {
                write!(f, "invalid device access width {width:?}")
            }
            Self::AddressOutOfRange { kind, address } => {
                write!(
                    f,
                    "{kind:?} address {address:?} is outside the selected device resource"
                )
            }
            Self::ReadOnly { kind, address } => {
                write!(f, "write to read-only {kind:?} address {address:?}")
            }
            Self::WriteOnly { kind, address } => {
                write!(f, "read from write-only {kind:?} address {address:?}")
            }
            Self::UnsupportedOperation => write!(f, "unsupported device operation"),
            Self::Backend(error) => write!(f, "device backend error: {error}"),
        }
    }
}

/// Result type for the new device abstraction layer.
pub type DeviceResult<T = ()> = Result<T, DeviceError>;

/// Unified interface for an emulated device instance.
pub trait DeviceOps: Any {
    /// Returns the registry-local device identifier.
    fn id(&self) -> DeviceId;

    /// Returns a human-readable device instance name.
    fn name(&self) -> &str;

    /// Returns all resources occupied or requested by this device.
    fn resources(&self) -> &[Resource];

    /// Returns optional capabilities supported by this device.
    fn capabilities(&self) -> DeviceCapabilities;

    /// Handles a normalized bus access.
    fn access(&self, access: BusAccess) -> DeviceResult<BusResponse>;

    /// Resets the device state.
    fn reset(&self) -> DeviceResult {
        Ok(())
    }

    /// Suspends the device state.
    fn suspend(&self) -> DeviceResult {
        Ok(())
    }

    /// Resumes the device state.
    fn resume(&self) -> DeviceResult {
        Ok(())
    }
}

/// Build-time context provided to device factories.
pub trait DeviceBuildContext {
    /// Allocates a registry-local device identifier.
    fn alloc_device_id(&mut self) -> DeviceId;
}

/// Factory that constructs native device model instances from VM configuration.
pub trait DeviceFactory: Sync {
    /// Returns the emulated device type handled by this factory.
    fn ty(&self) -> EmulatedDeviceType;

    /// Builds one or more native device instances from a VM device config.
    fn build(
        &self,
        ctx: &mut dyn DeviceBuildContext,
        config: &EmulatedDeviceConfig,
    ) -> DeviceResult<Vec<Rc<dyn DeviceOps>>>;
}
