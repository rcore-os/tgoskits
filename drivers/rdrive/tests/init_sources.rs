use rdrive::{
    DriverGeneric, Platform, PlatformDevice, PlatformSource, get_one, init_sources,
    probe::{OnProbeError, acpi::AcpiRoot},
    probe_all,
    register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority},
};

struct MixedSourceDevice;

impl DriverGeneric for MixedSourceDevice {
    fn name(&self) -> &str {
        "MixedSourceDevice"
    }
}

fn probe_static(plat_dev: PlatformDevice) -> Result<(), OnProbeError> {
    plat_dev.register(MixedSourceDevice);
    Ok(())
}

static STATIC_REGISTER: DriverRegister = DriverRegister {
    name: "mixed source static driver",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Static {
        on_probe: probe_static,
    }],
};

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
fn unsupported_source_does_not_leave_static_backend_initialized() {
    let err = init_sources(&[
        PlatformSource::Static,
        PlatformSource::Acpi(AcpiRoot::identity(0)),
    ])
    .expect_err("invalid acpi should reject the source set before committing static state");
    assert!(matches!(err, rdrive::error::DriverError::Unknown(_)));

    rdrive::init(Platform::Static).expect("static platform should init");
    rdrive::register_add(STATIC_REGISTER.clone());
    probe_all(true).expect("static probe should succeed");

    assert!(get_one::<MixedSourceDevice>().is_some());
}
