//! Data-only device models used before firmware construction.

mod error;
mod fingerprint;
mod registry;

use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};
pub use error::DeviceModelError;
pub use fingerprint::DeviceModelFingerprint;
pub use registry::DeviceModelRegistry;
use registry::EmptyDeviceModel;

use crate::{DeviceManagerResult, DeviceRequirements};

/// Declares resources without constructing a runtime device.
pub trait DeviceModel: Send + Sync {
    /// Returns the configuration type handled by this model.
    fn device_type(&self) -> EmulatedDeviceType;

    /// Validates the internal configuration and declares all named resources.
    fn requirements(
        &self,
        config: &EmulatedDeviceConfig,
    ) -> DeviceManagerResult<DeviceRequirements>;
}

/// Registers models that do not depend on an architecture backend.
pub fn register_builtin_models(registry: &mut DeviceModelRegistry) -> DeviceManagerResult {
    registry.register(alloc::sync::Arc::new(EmptyDeviceModel::new(
        EmulatedDeviceType::Dummy,
    )))?;
    registry.register(alloc::sync::Arc::new(EmptyDeviceModel::new(
        EmulatedDeviceType::IVCChannel,
    )))
}
