//! AML device descriptions rebuilt from finalized passthrough resources.

use alloc::{boxed::Box, format, string::String, vec, vec::Vec};

use acpi_tables::{
    Aml,
    aml::{
        AddressSpace, AddressSpaceCacheable, AmlString, Device, Interrupt, Name, ResourceTemplate,
    },
};
use axvm_types::InterruptTriggerMode;

use crate::machine::{
    DeviceDisposition, HostInterruptResource, HostInterruptSource, MachinePlanError,
    MachinePlanResult, PlannedHostDevice, VmMachinePlan,
};

/// Appends one AML device for every physical device assigned by the plan.
pub(super) fn append_passthrough_devices_aml(
    aml: &mut Vec<u8>,
    plan: &VmMachinePlan,
) -> MachinePlanResult<()> {
    for (index, device) in plan
        .host_devices()
        .iter()
        .filter(|device| device.disposition() == DeviceDisposition::Passthrough)
        .enumerate()
    {
        append_passthrough_device_aml(aml, index, device)?;
    }
    Ok(())
}

fn append_passthrough_device_aml(
    aml: &mut Vec<u8>,
    index: usize,
    device: &PlannedHostDevice,
) -> MachinePlanResult<()> {
    let name = acpi_device_name(index)?;
    let hid =
        device
            .compatibles()
            .first()
            .cloned()
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!(
                    "passthrough ACPI device '{}' has no hardware or compatible identifier",
                    device.id()
                ),
            })?;

    let mut resources: Vec<Box<dyn Aml>> = Vec::new();
    for range in device.mmio() {
        resources.push(Box::new(AddressSpace::new_memory(
            AddressSpaceCacheable::NotCacheable,
            true,
            range.base(),
            range.end() - 1,
            None,
        )));
    }
    for range in device.pio() {
        let end =
            u16::try_from(range.end() - 1).map_err(|_| MachinePlanError::InvalidFirmware {
                detail: format!(
                    "passthrough ACPI device '{}' has an invalid PIO end {:#x}",
                    device.id(),
                    range.end()
                ),
            })?;
        resources.push(Box::new(AddressSpace::new_io(range.base(), end, None)));
    }
    for interrupt in device.interrupts() {
        resources.push(Box::new(acpi_interrupt(device, interrupt)?));
    }
    if resources.is_empty() {
        return Err(MachinePlanError::InvalidFirmware {
            detail: format!(
                "passthrough ACPI device '{}' has no representable resources",
                device.id()
            ),
        });
    }

    let resource_refs = resources
        .iter()
        .map(|resource| resource.as_ref() as &dyn Aml)
        .collect::<Vec<_>>();
    let resource_template = ResourceTemplate::new(resource_refs);
    let hid: AmlString = hid;
    let hid_name = Name::new("_HID".into(), &hid);
    let uid = u32::try_from(index).map_err(|_| MachinePlanError::InvalidFirmware {
        detail: "passthrough ACPI device UID exceeds u32".into(),
    })?;
    let uid_name = Name::new("_UID".into(), &uid);
    let crs_name = Name::new("_CRS".into(), &resource_template);
    Device::new(name.as_str().into(), vec![&hid_name, &uid_name, &crs_name]).to_aml_bytes(aml);
    Ok(())
}

fn acpi_interrupt(
    device: &PlannedHostDevice,
    interrupt: &HostInterruptResource,
) -> MachinePlanResult<Interrupt> {
    let (gsi, active_low) = match interrupt.source() {
        HostInterruptSource::ControllerInput => (interrupt.input_u32(), false),
        HostInterruptSource::AcpiGsiRoute(route) => (
            interrupt.input_u32(),
            route.polarity == irq_framework::AcpiIrqPolarity::ActiveLow,
        ),
        HostInterruptSource::Fdt { .. } => {
            return Err(MachinePlanError::InvalidFirmware {
                detail: format!(
                    "passthrough device '{}' has an FDT interrupt that cannot be emitted as ACPI",
                    device.id()
                ),
            });
        }
    };
    Ok(Interrupt::new(
        true,
        interrupt.trigger() == InterruptTriggerMode::EdgeTriggered,
        active_low,
        false,
        gsi,
    ))
}

fn acpi_device_name(index: usize) -> MachinePlanResult<String> {
    if index > u8::MAX as usize {
        return Err(MachinePlanError::InvalidFirmware {
            detail: "ACPI namespace supports at most 256 passthrough devices".into(),
        });
    }
    Ok(format!("_SB_.PT{index:02X}"))
}
