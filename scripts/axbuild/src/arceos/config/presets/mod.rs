//! Built-in ArceOS recipes; input decoding remains owned by the request resolver.

mod entries;

use ax_config::CommonPreset;
pub(crate) use entries::PRESETS;

use super::ArceosConfig;

/// Borrowed OS policy expanded only for the current request.
#[derive(Debug)]
pub(crate) struct ArceosPreset {
    pub name: &'static str,
    pub common: CommonPreset,
    pub package: &'static str,
    pub to_bin: bool,
    pub freestanding: bool,
    pub app_c: Option<&'static str>,
}

impl ArceosPreset {
    pub fn materialize(&self) -> ArceosConfig {
        ArceosConfig {
            common: self.common.materialize(),
            package: Some(self.package.to_owned()),
            to_bin: self.to_bin,
            freestanding: self.freestanding,
            app_c: self.app_c.map(Into::into),
            ..Default::default()
        }
    }
}

pub(crate) fn find_preset(name: &str) -> Option<&'static ArceosPreset> {
    PRESETS.iter().copied().find(|preset| preset.name == name)
}

pub(crate) fn default_qemu_preset(target: &str) -> Option<&'static ArceosPreset> {
    use ax_config::boards::*;
    match target {
        TARGET_AARCH64 => Some(&entries::QEMU_AARCH64),
        TARGET_RISCV64 => Some(&entries::QEMU_RISCV64),
        TARGET_LOONGARCH64 => Some(&entries::QEMU_LOONGARCH64),
        TARGET_X86_64 => Some(&entries::QEMU_X86_64),
        _ => None,
    }
}
