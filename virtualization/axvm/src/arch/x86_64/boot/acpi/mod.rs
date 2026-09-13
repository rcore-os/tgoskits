//! x86 guest ACPI composed from the resolved VM device graph.

mod aml;
mod config;
mod fw_cfg;
mod serial;
mod tables;

#[cfg(all(test, feature = "host-fs"))]
pub(super) use aml::build_dsdt;
pub(crate) use config::{X86FirmwarePlan, X86PciIntxRoute};
pub(super) use fw_cfg::build_fw_cfg_blobs;
pub(super) use tables::build_direct_image;
