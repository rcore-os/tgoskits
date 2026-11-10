use ax_config::{CommonDefaults, CommonPreset, LogLevel, boards};

use super::AxvisorPreset;

pub(super) static ASUS_NUC15CRH_X86_64: AxvisorPreset = AxvisorPreset {
    name: "asus-nuc15crh-x86_64",
    common: CommonPreset::new(
        &boards::ASUS_NUC15CRH_X86_64,
        CommonDefaults {
            log: LogLevel::Info,
            ..CommonDefaults::DEFAULT
        },
    )
    .with_env(&[
        ("AX_IP", "10.0.2.15"),
        ("AX_GW", "10.0.2.2"),
        ("RDRIVE_ACPI_LOAD_AML", "0"),
    ]),
    vm_configs: &[],
};
