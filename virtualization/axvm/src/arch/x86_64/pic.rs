//! Device-graph factory for the x86 guest-owned legacy PIC pair.

use alloc::sync::Arc;

use axdevice::*;

pub(super) fn model() -> Arc<dyn DeviceModel> {
    Arc::new(X86PicModel)
}

struct X86PicModel;

impl DeviceModel for X86PicModel {
    fn declare(&self) -> DeviceManagerResult<DeviceDeclaration> {
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

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
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
