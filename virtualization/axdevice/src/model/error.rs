//! Structured device-model registry failures.

use alloc::string::String;

use axvm_types::EmulatedDeviceType;

use super::DeviceModelFingerprint;

/// Failure while selecting or validating a pure device model.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DeviceModelError {
    /// A model type was registered more than once.
    #[error("device model for {device_type} is already registered")]
    DuplicateModel {
        /// Duplicate device type.
        device_type: EmulatedDeviceType,
    },
    /// No model exists for a configured device.
    #[error("device {device_id} requires an unregistered {device_type} model")]
    MissingModel {
        /// Stable device identifier.
        device_id: String,
        /// Missing device type.
        device_type: EmulatedDeviceType,
    },
    /// Planning and construction did not use the same model input.
    #[error(
        "device {device_id} model fingerprint changed from {planned} to {current} between \
         planning and construction"
    )]
    FingerprintMismatch {
        /// Stable device identifier.
        device_id: String,
        /// Fingerprint captured by the plan.
        planned: DeviceModelFingerprint,
        /// Fingerprint recomputed before construction.
        current: DeviceModelFingerprint,
    },
}
