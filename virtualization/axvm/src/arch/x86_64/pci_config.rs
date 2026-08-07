//! Device-graph factory for the x86 PCI configuration port window.

use axdevice::*;

pub(super) struct X86PciConfigModel;

impl DeviceModel for X86PciConfigModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_pio(
            ResourceSlot::new("registers")?,
            axdevice::X86PciConfigDevice::PORT_SIZE,
            1,
            ResourceRequest::Fixed(axdevice::X86PciConfigDevice::PORT_BASE),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
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
            std::sync::Arc::new(axdevice::X86PciConfigDevice::new()),
        )))
    }
}
