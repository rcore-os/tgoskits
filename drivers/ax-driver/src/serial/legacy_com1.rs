use alloc::string::String;

use log::info;
use rdrive::{
    PlatformDevice,
    probe::{OnProbeError, acpi::AcpiResourceAddress},
    register::{ProbeKind, ProbeLevel, ProbePriority},
};
use some_serial::ns16550::Ns16550;

use super::{BindingIrq, PlatformSerialDevice, erase_uart};

const COM1_PORT: u16 = 0x3f8;
const COM1_ISA_IRQ: u8 = 4;

model_register!(
    name: "legacy x86 COM1 serial",
    level: ProbeLevel::PreKernel,
    priority: ProbePriority(ProbePriority::DEFAULT.0 + 1),
    probe_kinds: &[ProbeKind::Static { on_probe: probe }],
);

fn probe(platform: PlatformDevice) -> Result<(), OnProbeError> {
    let resource = AcpiResourceAddress::io(u64::from(COM1_PORT));
    if rdrive::acpi_resource_address_to_device_id(resource).is_some() {
        return Err(OnProbeError::NotMatch);
    }
    let Some(early) = someboot::console::early_ns16550_port() else {
        return Err(OnProbeError::NotMatch);
    };
    if early.port != COM1_PORT {
        return Err(OnProbeError::NotMatch);
    }
    let route =
        rdrive::probe::acpi::with_acpi(|system| system.routing().resolve_isa_irq(COM1_ISA_IRQ))
            .flatten()
            .ok_or_else(|| OnProbeError::other("legacy COM1 IRQ4 has no ACPI interrupt route"))?;
    let device_id = platform.descriptor().device_id();
    let device = PlatformSerialDevice::new(
        erase_uart(Ns16550::new_port(COM1_PORT, early.input_clock_hz)),
        String::from("legacy-com1"),
        Some(0),
        usize::from(COM1_PORT),
        Some(BindingIrq::from(route)),
    );
    if rdrive::probe::acpi::with_acpi(|system| {
        system.associate_resource_address(resource, device_id)
    }) != Some(Ok(()))
    {
        return Err(OnProbeError::NotMatch);
    }
    platform.register(device);
    info!("legacy x86 COM1@0x3f8 registered successfully");
    Ok(())
}
