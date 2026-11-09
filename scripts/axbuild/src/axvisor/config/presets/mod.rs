//! Built-in Axvisor recipes; VM lists are packaging inputs, not Cargo identity.

mod entries;

use ax_config::CommonPreset;
pub(crate) use entries::PRESETS;

use super::AxvisorConfig;

/// Borrowed OS policy expanded only for the current request.
#[derive(Debug)]
pub(crate) struct AxvisorPreset {
    pub name: &'static str,
    pub common: CommonPreset,
    pub vm_configs: &'static [&'static str],
}

impl AxvisorPreset {
    pub fn materialize(&self) -> AxvisorConfig {
        AxvisorConfig {
            common: self.common.materialize(),
            vm_configs: self.vm_configs.iter().map(Into::into).collect(),
            ..Default::default()
        }
    }
}

pub(crate) fn find_preset(name: &str) -> Option<&'static AxvisorPreset> {
    PRESETS.iter().copied().find(|preset| preset.name == name)
}

pub(crate) fn default_qemu_preset(target: &str) -> Option<&'static AxvisorPreset> {
    use ax_config::boards::*;
    match target {
        TARGET_AARCH64 => Some(&entries::QEMU_AARCH64),
        TARGET_RISCV64 => Some(&entries::QEMU_RISCV64),
        TARGET_LOONGARCH64 => Some(&entries::QEMU_LOONGARCH64),
        TARGET_X86_64 => Some(&entries::QEMU_X86_64),
        _ => None,
    }
}
