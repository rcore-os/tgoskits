//! Device-graph factory for the x86 firmware ACPI PM timer.

use alloc::sync::Arc;

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceDeclaration, DeviceFactory, DeviceFactoryRegistry,
    DeviceManagerError, DeviceManagerResult, DeviceRegistration, DeviceRequirements,
    ResourceRequest, ResourceSlot, validate_device_config,
};
use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{AxVmError, AxVmResult};

pub(super) fn register_factory(
    configs: &[EmulatedDeviceConfig],
    factories: &mut DeviceFactoryRegistry,
) -> AxVmResult {
    let mut matches = configs
        .iter()
        .filter(|config| config.emu_type == EmulatedDeviceType::X86AcpiPmTimer);
    let Some(expected) = matches.next() else {
        return Ok(());
    };
    if matches.next().is_some() {
        return Err(AxVmError::invalid_config(
            "x86 machine profile has more than one ACPI PM timer",
        ));
    }
    factories.register(Arc::new(X86AcpiPmTimerFactory {
        expected: expected.clone(),
    }))?;
    Ok(())
}

struct X86AcpiPmTimerFactory {
    expected: EmulatedDeviceConfig,
}

impl DeviceFactory for X86AcpiPmTimerFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::X86AcpiPmTimer
    }

    fn declare(&self, config: &EmulatedDeviceConfig) -> DeviceManagerResult<DeviceDeclaration> {
        validate_device_config(&self.expected, config, "declare x86 ACPI PM timer")?;
        let size = u16::try_from(config.length).map_err(range_error)?;
        let base = u16::try_from(config.base_gpa).map_err(range_error)?;
        DeviceRequirements::new()
            .with_pio(
                ResourceSlot::new("registers")?,
                size,
                1,
                ResourceRequest::Fixed(base),
            )?
            .with_wired_irq(
                ResourceSlot::new("sci")?,
                InterruptControllerId::new(0),
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
                ResourceRequest::Fixed(ControllerInputId::new(config.irq_id)),
            )
            .map(DeviceDeclaration::with_requirements)
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        validate_device_config(&self.expected, config, "build x86 ACPI PM timer")?;
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

fn range_error(_error: core::num::TryFromIntError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "declare x86 ACPI PM timer",
        detail: "ACPI PM timer port range exceeds 16 bits".into(),
    }
}
