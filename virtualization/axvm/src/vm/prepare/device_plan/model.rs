//! Reusable data-only models for fixed machine-profile resources.

use axdevice::{
    DeviceManagerError, DeviceManagerResult, DeviceModel, DeviceRequirements, ResourceRequest,
    ResourceSlot,
};
use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

/// Address-space kind consumed by one internal machine device.
#[derive(Clone, Copy)]
pub(crate) enum FixedAddressKind {
    Mmio,
    Pio,
}

/// Model for one internal device type whose ABI values come from its config.
pub(crate) struct FixedDeviceModel {
    device_type: EmulatedDeviceType,
    address: FixedAddressKind,
    wired: Option<WiredRequirement>,
}

#[derive(Clone, Copy)]
struct WiredRequirement {
    controller: InterruptControllerId,
    trigger: InterruptTrigger,
    sharing: InterruptSharing,
}

impl FixedDeviceModel {
    pub(crate) const fn new(device_type: EmulatedDeviceType, address: FixedAddressKind) -> Self {
        Self {
            device_type,
            address,
            wired: None,
        }
    }

    pub(crate) const fn with_wired_irq(
        mut self,
        controller: InterruptControllerId,
        trigger: InterruptTrigger,
        sharing: InterruptSharing,
    ) -> Self {
        self.wired = Some(WiredRequirement {
            controller,
            trigger,
            sharing,
        });
        self
    }
}

impl DeviceModel for FixedDeviceModel {
    fn device_type(&self) -> EmulatedDeviceType {
        self.device_type
    }

    fn requirements(
        &self,
        config: &EmulatedDeviceConfig,
    ) -> DeviceManagerResult<DeviceRequirements> {
        if config.emu_type != self.device_type {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "declare internal device resources",
                detail: alloc::format!(
                    "{} model received {} configuration",
                    self.device_type,
                    config.emu_type
                ),
            });
        }

        let mut requirements = DeviceRequirements::new();
        requirements = match self.address {
            FixedAddressKind::Mmio => requirements.with_mmio(
                registers_slot()?,
                u64::try_from(config.length).map_err(|_| address_conversion_error(config))?,
                1,
                ResourceRequest::Fixed(
                    u64::try_from(config.base_gpa).map_err(|_| address_conversion_error(config))?,
                ),
            )?,
            FixedAddressKind::Pio => requirements.with_pio(
                registers_slot()?,
                u16::try_from(config.length).map_err(|_| address_conversion_error(config))?,
                1,
                ResourceRequest::Fixed(
                    u16::try_from(config.base_gpa).map_err(|_| address_conversion_error(config))?,
                ),
            )?,
        };
        if let Some(wired) = self.wired {
            requirements = requirements.with_wired_irq(
                irq_slot()?,
                wired.controller,
                wired.trigger,
                wired.sharing,
                ResourceRequest::Fixed(ControllerInputId::new(config.irq_id)),
            )?;
        }
        Ok(requirements)
    }
}

fn registers_slot() -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new("registers")
}

fn irq_slot() -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new("irq")
}

fn address_conversion_error(config: &EmulatedDeviceConfig) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "declare internal device resources",
        detail: alloc::format!(
            "device {} address {:#x} or length {:#x} exceeds its bus width",
            config.name,
            config.base_gpa,
            config.length
        ),
    }
}
