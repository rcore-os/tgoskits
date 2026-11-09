//! Built-in StarryOS recipes and their explicit packaging resources.

mod entries;

use ax_config::CommonPreset;
pub(crate) use entries::PRESETS;

use super::StarryConfig;

/// Borrowed OS policy expanded only for the current request.
#[derive(Debug)]
pub(crate) struct StarryPreset {
    pub name: &'static str,
    pub common: CommonPreset,
    pub fit_its: Option<&'static str>,
}

impl StarryPreset {
    pub fn materialize(&self) -> StarryConfig {
        StarryConfig {
            common: self.common.materialize(),
            fit_its: self.fit_its.map(Into::into),
            ..Default::default()
        }
    }
}

pub(crate) fn find_preset(name: &str) -> Option<&'static StarryPreset> {
    PRESETS.iter().copied().find(|preset| preset.name == name)
}

pub(crate) fn default_qemu_preset(target: &str) -> Option<&'static StarryPreset> {
    use ax_config::boards::*;
    match target {
        TARGET_AARCH64 => Some(&entries::QEMU_AARCH64),
        TARGET_RISCV64 => Some(&entries::QEMU_RISCV64),
        TARGET_LOONGARCH64 => Some(&entries::QEMU_LOONGARCH64),
        TARGET_X86_64 => Some(&entries::QEMU_X86_64),
        _ => None,
    }
}
