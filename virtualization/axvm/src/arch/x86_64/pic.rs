//! Device-graph factory for the x86 guest-owned legacy PIC pair.

use alloc::sync::Arc;

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceDeclaration, DeviceFactory, DeviceFactoryRegistry,
    DeviceManagerError, DeviceManagerResult, DeviceRegistration, DeviceRequirements,
    ResourceRequest, ResourceSlot, X86PicDeviceOps, X86PicServiceKey, validate_device_config,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{AxVmError, AxVmResult};

pub(super) fn register_factory(
    configs: &[EmulatedDeviceConfig],
    factories: &mut DeviceFactoryRegistry,
) -> AxVmResult {
    let mut matches = configs
        .iter()
        .filter(|config| config.emu_type == EmulatedDeviceType::X86Pic);
    let Some(expected) = matches.next() else {
        return Ok(());
    };
    if matches.next().is_some() {
        return Err(AxVmError::invalid_config(
            "x86 machine profile has more than one legacy PIC device",
        ));
    }
    factories.register(Arc::new(X86PicFactory {
        expected: expected.clone(),
    }))?;
    Ok(())
}

struct X86PicFactory {
    expected: EmulatedDeviceConfig,
}

impl DeviceFactory for X86PicFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::X86Pic
    }

    fn declare(&self, config: &EmulatedDeviceConfig) -> DeviceManagerResult<DeviceDeclaration> {
        validate_device_config(&self.expected, config, "declare x86 legacy PIC")?;
        DeviceRequirements::new()
            .with_pio(
                ResourceSlot::new("master-registers")?,
                2,
                1,
                ResourceRequest::Fixed(0x20),
            )?
            .with_pio(
                ResourceSlot::new("slave-registers")?,
                2,
                1,
                ResourceRequest::Fixed(0xa0),
            )
            .map(DeviceDeclaration::with_requirements)
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        validate_device_config(&self.expected, config, "build x86 legacy PIC")?;
        let master = context.pio(&ResourceSlot::new("master-registers")?)?;
        let slave = context.pio(&ResourceSlot::new("slave-registers")?)?;
        if master != (0x20, 2) || slave != (0xa0, 2) {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 legacy PIC",
                detail: "planned PIC ranges must be 0x20..=0x21 and 0xa0..=0xa1".into(),
            });
        }
        let pic = Arc::new(axdevice::X86PicDevice::new());
        let service: Arc<dyn X86PicDeviceOps> = pic.clone();
        DeviceBundle::from_registration(DeviceRegistration::Device(pic))
            .with_service::<X86PicServiceKey>(service)
    }
}
