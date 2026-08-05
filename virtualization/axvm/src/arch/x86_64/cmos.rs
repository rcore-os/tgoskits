//! Device-graph factory for the x86 CMOS platform device.

use alloc::sync::Arc;

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceDeclaration, DeviceFactory, DeviceFactoryRegistry,
    DeviceManagerError, DeviceManagerResult, DeviceRegistration, DeviceRequirements,
    ResourceRequest, ResourceSlot, validate_device_config,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, VmMemMappingType};

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

pub(super) fn register_factory(
    configs: &[EmulatedDeviceConfig],
    low_memory_size: u64,
    factories: &mut DeviceFactoryRegistry,
) -> AxVmResult {
    let mut matches = configs
        .iter()
        .filter(|config| config.emu_type == EmulatedDeviceType::X86Cmos);
    let Some(expected) = matches.next() else {
        return Ok(());
    };
    if matches.next().is_some() {
        return Err(crate::AxVmError::invalid_config(
            "x86 machine profile has more than one CMOS device",
        ));
    }
    factories.register(Arc::new(X86CmosFactory {
        expected: expected.clone(),
        low_memory_size,
    }))?;
    Ok(())
}

struct X86CmosFactory {
    expected: EmulatedDeviceConfig,
    low_memory_size: u64,
}

impl DeviceFactory for X86CmosFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::X86Cmos
    }

    fn declare(&self, config: &EmulatedDeviceConfig) -> DeviceManagerResult<DeviceDeclaration> {
        validate_device_config(&self.expected, config, "declare x86 CMOS")?;
        let size = u16::try_from(config.length).map_err(range_error)?;
        let base = u16::try_from(config.base_gpa).map_err(range_error)?;
        DeviceRequirements::new()
            .with_pio(
                ResourceSlot::new("registers")?,
                size,
                1,
                ResourceRequest::Fixed(base),
            )
            .map(DeviceDeclaration::with_requirements)
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        validate_device_config(&self.expected, config, "build x86 CMOS")?;
        let range = context.pio(&ResourceSlot::new("registers")?)?;
        if range != (0x70, 2) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 CMOS",
                detail: "planned CMOS range must be 0x70..=0x71".into(),
            });
        }
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            Arc::new(axdevice::X86CmosDevice::new(self.low_memory_size)),
        )))
    }
}

fn range_error(_error: core::num::TryFromIntError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "declare x86 CMOS",
        detail: "CMOS port range exceeds 16 bits".into(),
    }
}
