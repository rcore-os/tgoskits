use rdrive::{
    DriverGeneric, Platform, PlatformDevice, PlatformSource, get_one, init_sources,
    probe::{OnProbeError, acpi::AcpiRoot},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

struct StaticTestDevice;

impl DriverGeneric for StaticTestDevice {
    fn name(&self) -> &str {
        "StaticTestDevice"
    }
}

fn probe_static(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    plat_dev.register(StaticTestDevice);
    Ok(())
}

static STATIC_REGISTER: DriverRegister = DriverRegister {
    name: "static test driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_static,
    }],
};

#[test]
fn static_probe_registers_device() {
    rdrive::init(Platform::Static).expect("static platform should init");
    rdrive::register_add(STATIC_REGISTER.clone());

    probe_all(true).expect("static probe should succeed");

    assert!(get_one::<StaticTestDevice>().is_some());
}

#[test]
fn acpi_source_is_unsupported() {
    let err = rdrive::init(Platform::Acpi(AcpiRoot::identity(0)))
        .expect_err("invalid acpi should be rejected");

    assert!(matches!(err, rdrive::error::DriverError::Unknown(_)));
}

#[test]
fn fdt_phandle_lookup_is_none_without_fdt_source() {
    init_sources(&[PlatformSource::Static]).expect("static source should init");

    assert!(rdrive::fdt_phandle_to_device_id(1.into()).is_none());
}

#[test]
fn acpi_ioapic_routes_map_gsi_to_stable_vector() {
    use rdrive::probe::acpi::{AcpiIoApic, AcpiIrqPolarity, AcpiIrqTrigger, AcpiRouting};

    let mut routing = AcpiRouting::new();
    routing.add_io_apic(AcpiIoApic {
        id: 0,
        address: 0xfec0_0000,
        gsi_base: 0,
        redirection_entries: 24,
    });

    let irq = routing
        .resolve_gsi(16)
        .expect("gsi 16 should be handled by the IOAPIC");
    assert_eq!(irq.gsi, 16);
    assert_eq!(irq.controller_id, 0);
    assert_eq!(irq.controller_address, 0xfec0_0000);
    assert_eq!(irq.controller_input, 16);
    assert_eq!(irq.vector, 0x40);
    assert_eq!(irq.trigger, AcpiIrqTrigger::Level);
    assert_eq!(irq.polarity, AcpiIrqPolarity::ActiveLow);
    assert!(routing.resolve_gsi(24).is_none());
}
