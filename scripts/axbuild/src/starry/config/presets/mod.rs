//! Built-in StarryOS recipes and their explicit packaging resources.

mod aka_00_sg2002;
mod jl_lsgd2k10;
mod k230_canmv;
mod licheerv_nano_sg2002;
mod ls2k1000;
mod orangepi_5_plus;
mod phytiumpi;
mod qemu_aarch64;
mod qemu_loongarch64;
mod qemu_riscv64;
mod qemu_x86_64;
mod roc_rk3568_pc;
mod rock_4d;
mod visionfive2;

use ax_config::CommonPreset;

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

pub(crate) static PRESETS: &[&StarryPreset] = &[
    &aka_00_sg2002::AKA_00_SG2002,
    &jl_lsgd2k10::JL_LSGD2K10,
    &k230_canmv::K230_CANMV,
    &licheerv_nano_sg2002::LICHEERV_NANO_SG2002_WIFI,
    &licheerv_nano_sg2002::LICHEERV_NANO_SG2002,
    &ls2k1000::LS2K1000,
    &orangepi_5_plus::ORANGEPI_5_PLUS,
    &phytiumpi::PHYTIUMPI,
    &qemu_aarch64::QEMU_AARCH64,
    &qemu_loongarch64::QEMU_LOONGARCH64_UEFI,
    &qemu_loongarch64::QEMU_LOONGARCH64,
    &qemu_riscv64::QEMU_RISCV64,
    &qemu_x86_64::QEMU_X86_64,
    &roc_rk3568_pc::ROC_RK3568_PC,
    &rock_4d::ROCK_4D,
    &visionfive2::VISIONFIVE2,
];

pub(crate) fn find_preset(name: &str) -> Option<&'static StarryPreset> {
    PRESETS.iter().copied().find(|preset| preset.name == name)
}

pub(crate) fn default_qemu_preset(target: &str) -> Option<&'static StarryPreset> {
    use ax_config::boards::*;
    match target {
        TARGET_AARCH64 => Some(&qemu_aarch64::QEMU_AARCH64),
        TARGET_RISCV64 => Some(&qemu_riscv64::QEMU_RISCV64),
        TARGET_LOONGARCH64 => Some(&qemu_loongarch64::QEMU_LOONGARCH64),
        TARGET_X86_64 => Some(&qemu_x86_64::QEMU_X86_64),
        _ => None,
    }
}
