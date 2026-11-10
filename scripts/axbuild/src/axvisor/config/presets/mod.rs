//! Built-in Axvisor recipes; VM lists are packaging inputs, not Cargo identity.

mod asus_nuc15crh_x86_64;
mod orangepi_5_plus;
mod phytiumpi;
mod qemu_aarch64;
mod qemu_loongarch64;
mod qemu_riscv64;
mod qemu_x86_64;
mod rdk_s100;
mod roc_rk3568_pc;
mod rock_4d;
mod tac_e400;

use ax_config::CommonPreset;

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

pub(crate) static PRESETS: &[&AxvisorPreset] = &[
    &asus_nuc15crh_x86_64::ASUS_NUC15CRH_X86_64,
    &orangepi_5_plus::ORANGEPI_5_PLUS,
    &phytiumpi::PHYTIUMPI,
    &qemu_aarch64::QEMU_AARCH64_VCPUS,
    &qemu_aarch64::QEMU_AARCH64_VIRTIO_BLK_TEST,
    &qemu_aarch64::QEMU_AARCH64_VIRTIO_NET_PEER,
    &qemu_aarch64::QEMU_AARCH64,
    &qemu_loongarch64::QEMU_LOONGARCH64,
    &qemu_riscv64::QEMU_RISCV64,
    &qemu_x86_64::QEMU_X86_64_LINUX,
    &qemu_x86_64::QEMU_X86_64,
    &rdk_s100::RDK_S100,
    &roc_rk3568_pc::ROC_RK3568_PC,
    &rock_4d::ROCK_4D,
    &tac_e400::TAC_E400,
];

pub(crate) fn find_preset(name: &str) -> Option<&'static AxvisorPreset> {
    PRESETS.iter().copied().find(|preset| preset.name == name)
}

pub(crate) fn default_qemu_preset(target: &str) -> Option<&'static AxvisorPreset> {
    use ax_config::boards::*;
    match target {
        TARGET_AARCH64 => Some(&qemu_aarch64::QEMU_AARCH64),
        TARGET_RISCV64 => Some(&qemu_riscv64::QEMU_RISCV64),
        TARGET_LOONGARCH64 => Some(&qemu_loongarch64::QEMU_LOONGARCH64),
        TARGET_X86_64 => Some(&qemu_x86_64::QEMU_X86_64),
        _ => None,
    }
}
