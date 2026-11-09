//! Built-in ArceOS recipes; input decoding remains owned by the request resolver.

mod orangepi_5_plus;
mod qemu_aarch64;
mod qemu_loongarch64;
mod qemu_riscv64;
mod qemu_x86_64;

use ax_config::CommonPreset;

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

pub(crate) static PRESETS: &[&ArceosPreset] = &[
    &orangepi_5_plus::ORANGEPI_5_PLUS,
    &qemu_aarch64::QEMU_AARCH64,
    &qemu_loongarch64::QEMU_LOONGARCH64,
    &qemu_riscv64::QEMU_RISCV64,
    &qemu_x86_64::QEMU_X86_64,
];

pub(crate) fn find_preset(name: &str) -> Option<&'static ArceosPreset> {
    PRESETS.iter().copied().find(|preset| preset.name == name)
}

pub(crate) fn default_qemu_preset(target: &str) -> Option<&'static ArceosPreset> {
    use ax_config::boards::*;
    match target {
        TARGET_AARCH64 => Some(&qemu_aarch64::QEMU_AARCH64),
        TARGET_RISCV64 => Some(&qemu_riscv64::QEMU_RISCV64),
        TARGET_LOONGARCH64 => Some(&qemu_loongarch64::QEMU_LOONGARCH64),
        TARGET_X86_64 => Some(&qemu_x86_64::QEMU_X86_64),
        _ => None,
    }
}
