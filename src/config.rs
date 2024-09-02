use alloc::vec::Vec;
use axdevice_base::EmulatedDeviceConfig;

/// The vector of DeviceConfig
pub struct AxVmDeviceConfig {
    /// The vector of EmulatedDeviceConfig
    pub emu_configs: Vec<EmulatedDeviceConfig>,
}

/// The implemention for AxVmDeviceConfig
impl AxVmDeviceConfig {
    /// The new function for AxVmDeviceConfig
    pub fn new(emu_configs: Vec<EmulatedDeviceConfig>) -> Self {
        Self { emu_configs }
    }
}
