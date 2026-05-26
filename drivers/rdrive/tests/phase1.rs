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
    let err =
        rdrive::init(Platform::Acpi(AcpiRoot { rsdp: 0 })).expect_err("acpi is not supported yet");

    assert!(matches!(
        err,
        rdrive::error::DriverError::Unsupported("acpi")
    ));
}

#[test]
fn fdt_phandle_lookup_is_none_without_fdt_source() {
    init_sources(&[PlatformSource::Static]).expect("static source should init");

    assert!(rdrive::fdt_phandle_to_device_id(1.into()).is_none());
}
