//! Device-graph factory for the x86 firmware ACPI PM timer.

use alloc::sync::Arc;

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceDeclaration, DeviceManagerError, DeviceManagerResult,
    DeviceModel, DeviceRegistration, DeviceRequirements, ResourceRequest, ResourceSlot,
};
use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};
pub(super) fn model() -> Arc<dyn DeviceModel> {
    Arc::new(X86AcpiPmTimerModel)
}

struct X86AcpiPmTimerModel;

impl DeviceModel for X86AcpiPmTimerModel {
    fn declare(&self) -> DeviceManagerResult<DeviceDeclaration> {
        DeviceRequirements::new()
            .with_pio(
                ResourceSlot::new("registers")?,
                axdevice::X86AcpiPmTimerDevice::PORT_SIZE,
                1,
                ResourceRequest::Fixed(axdevice::X86AcpiPmTimerDevice::PORT_BASE),
            )?
            .with_wired_irq(
                ResourceSlot::new("sci")?,
                InterruptControllerId::new(0),
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
                ResourceRequest::Fixed(ControllerInputId::new(9)),
            )
            .map(DeviceDeclaration::with_requirements)
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let range = context.pio(&ResourceSlot::new("registers")?)?;
        let expected = (
            axdevice::X86AcpiPmTimerDevice::PORT_BASE,
            axdevice::X86AcpiPmTimerDevice::PORT_SIZE,
        );
        if range != expected {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 ACPI PM timer",
                detail: "planned ICH9 PM range must be 0x600..=0x67f".into(),
            });
        }
        let sci = context.irq(&ResourceSlot::new("sci")?)?;
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            Arc::new(axdevice::X86AcpiPmTimerDevice::new(
                monotonic_time_nanos,
                sci,
            )?),
        )))
    }
}

fn monotonic_time_nanos() -> u64 {
    ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos()
}
