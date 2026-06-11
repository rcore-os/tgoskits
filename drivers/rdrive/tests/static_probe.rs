use rdrive::{
    DriverGeneric, Platform, PlatformDevice, get_one,
    probe::OnProbeError,
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
