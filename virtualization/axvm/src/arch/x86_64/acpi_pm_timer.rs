//! Device-graph factory for the x86 firmware ACPI PM timer.

use axdevice::*;
use axdevice_base::*;

pub(super) struct X86AcpiPmTimerModel;

impl DeviceModel for X86AcpiPmTimerModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
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
        let stop = StopGrant::new();
        let device: std::sync::Arc<dyn Device> = std::sync::Arc::new(
            axdevice::X86AcpiPmTimerDevice::new(monotonic_time_nanos, sci, stop.clone())?,
        );
        Ok(DeviceBundle::new().with_stop_device_grant(device, stop))
    }
}

fn monotonic_time_nanos() -> u64 {
    ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos()
}
