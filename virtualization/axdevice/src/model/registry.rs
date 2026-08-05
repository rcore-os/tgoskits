//! Registry for data-only device resource models.

use alloc::{sync::Arc, vec::Vec};

use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use super::{DeviceModel, DeviceModelError, DeviceModelFingerprint};
use crate::{DeviceManagerResult, DevicePlanRequest, DeviceRequirements, VmResourcePlan};

/// Pure model registry used before firmware and runtime construction.
#[derive(Default)]
pub struct DeviceModelRegistry {
    models: Vec<(EmulatedDeviceType, Arc<dyn DeviceModel>)>,
}

impl DeviceModelRegistry {
    /// Creates an empty registry.
    pub const fn new() -> Self {
        Self { models: Vec::new() }
    }

    /// Registers one authoritative model for a device type.
    pub fn register(&mut self, model: Arc<dyn DeviceModel>) -> DeviceManagerResult {
        let device_type = model.device_type();
        if self.get(device_type).is_some() {
            return Err(DeviceModelError::DuplicateModel { device_type }.into());
        }
        self.models.push((device_type, model));
        Ok(())
    }

    /// Returns the model registered for a device type.
    pub fn get(&self, device_type: EmulatedDeviceType) -> Option<&dyn DeviceModel> {
        self.models
            .iter()
            .find(|(registered, _)| *registered == device_type)
            .map(|(_, model)| model.as_ref())
    }

    /// Declares one stable planning request from an internal device config.
    pub fn plan_request(
        &self,
        device_id: &str,
        config: &EmulatedDeviceConfig,
    ) -> DeviceManagerResult<DevicePlanRequest> {
        let model = self.require(device_id, config.emu_type)?;
        let requirements = model.requirements(config)?;
        let fingerprint = DeviceModelFingerprint::for_model(config, &requirements);
        DevicePlanRequest::for_model(device_id, requirements, fingerprint)
    }

    /// Confirms that construction sees the exact model input used by planning.
    pub fn verify(
        &self,
        device_id: &str,
        config: &EmulatedDeviceConfig,
        plan: &VmResourcePlan,
    ) -> DeviceManagerResult {
        let current = self.plan_request(device_id, config)?.model_fingerprint();
        let planned = plan.model_fingerprint(device_id)?;
        if current != planned {
            return Err(DeviceModelError::FingerprintMismatch {
                device_id: device_id.into(),
                planned,
                current,
            }
            .into());
        }
        Ok(())
    }

    fn require(
        &self,
        device_id: &str,
        device_type: EmulatedDeviceType,
    ) -> DeviceManagerResult<&dyn DeviceModel> {
        self.get(device_type).ok_or_else(|| {
            DeviceModelError::MissingModel {
                device_id: device_id.into(),
                device_type,
            }
            .into()
        })
    }
}

pub(crate) struct EmptyDeviceModel {
    device_type: EmulatedDeviceType,
}

impl EmptyDeviceModel {
    pub(crate) const fn new(device_type: EmulatedDeviceType) -> Self {
        Self { device_type }
    }
}

impl DeviceModel for EmptyDeviceModel {
    fn device_type(&self) -> EmulatedDeviceType {
        self.device_type
    }

    fn requirements(
        &self,
        _config: &EmulatedDeviceConfig,
    ) -> DeviceManagerResult<DeviceRequirements> {
        Ok(DeviceRequirements::new())
    }
}
