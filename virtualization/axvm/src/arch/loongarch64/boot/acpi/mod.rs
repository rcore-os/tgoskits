//! LoongArch UEFI ACPI/AML blobs exposed through fw_cfg.

mod aml;
mod composer;
mod config;
mod tables;

pub(super) use composer::build;
pub(super) use config::{
    LoongArchFwCfgInterruptConfig, LoongArchFwCfgPciConfig, LoongArchFwCfgSerialConfig,
};
