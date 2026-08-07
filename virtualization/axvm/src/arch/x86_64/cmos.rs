//! Device-graph factory for the x86 CMOS platform device.

use axdevice::*;
use axvm_types::VmMemMappingType;

use crate::{AxVmResult, config::AxVMConfig};

pub(super) fn guest_low_memory_size(config: &AxVMConfig) -> AxVmResult<u64> {
    config
        .memory_regions()
        .iter()
        .filter(|region| region.map_type == VmMemMappingType::MapAlloc && region.gpa == 0)
        .map(|region| {
            u64::try_from(region.size)
                .map_err(|_| crate::AxVmError::invalid_config("x86 guest RAM size exceeds 64 bits"))
        })
        .next()
        .transpose()?
        .ok_or_else(|| crate::AxVmError::invalid_config("x86 firmware requires guest RAM at GPA 0"))
}

pub(super) struct X86CmosModel {
    low_memory_size: u64,
}

impl X86CmosModel {
    pub(super) const fn new(low_memory_size: u64) -> Self {
        Self { low_memory_size }
    }
}

impl DeviceModel for X86CmosModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_pio(
            ResourceSlot::new("registers")?,
            2,
            1,
            ResourceRequest::Fixed(0x70),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let range = context.pio(&ResourceSlot::new("registers")?)?;
        if range != (0x70, 2) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 CMOS",
                detail: "planned CMOS range must be 0x70..=0x71".into(),
            });
        }
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            std::sync::Arc::new(axdevice::X86CmosDevice::new(self.low_memory_size)),
        )))
    }
}
