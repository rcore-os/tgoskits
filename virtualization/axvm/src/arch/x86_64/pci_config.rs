//! Device-graph factory for the x86 PCI configuration port window.

use alloc::sync::Arc;

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceDeclaration, DeviceFactory, DeviceFactoryRegistry,
    DeviceManagerError, DeviceManagerResult, DeviceRegistration, DeviceRequirements,
    ResourceRequest, ResourceSlot, validate_device_config,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{AxVmError, AxVmResult};

pub(super) fn register_factory(
    configs: &[EmulatedDeviceConfig],
    factories: &mut DeviceFactoryRegistry,
) -> AxVmResult {
    let mut matches = configs
        .iter()
        .filter(|config| config.emu_type == EmulatedDeviceType::X86PciConfig);
    let Some(expected) = matches.next() else {
        return Ok(());
    };
    if matches.next().is_some() {
        return Err(AxVmError::invalid_config(
            "x86 machine profile has more than one PCI configuration device",
        ));
    }
    factories.register(Arc::new(X86PciConfigFactory {
        expected: expected.clone(),
    }))?;
    Ok(())
}

struct X86PciConfigFactory {
    expected: EmulatedDeviceConfig,
}

impl DeviceFactory for X86PciConfigFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::X86PciConfig
    }

    fn declare(&self, config: &EmulatedDeviceConfig) -> DeviceManagerResult<DeviceDeclaration> {
        validate_device_config(&self.expected, config, "declare x86 PCI configuration")?;
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
        validate_device_config(&self.expected, config, "build x86 PCI configuration")?;
        let range = context.pio(&ResourceSlot::new("registers")?)?;
        let expected = (
            axdevice::X86PciConfigDevice::PORT_BASE,
            axdevice::X86PciConfigDevice::PORT_SIZE,
        );
        if range != expected {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 PCI configuration",
                detail: "planned PCI configuration range must be 0xcf8..=0xcff".into(),
            });
        }
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            Arc::new(axdevice::X86PciConfigDevice::new()),
        )))
    }
}

fn range_error(_error: core::num::TryFromIntError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "declare x86 PCI configuration",
        detail: "PCI configuration port range exceeds 16 bits".into(),
    }
}
