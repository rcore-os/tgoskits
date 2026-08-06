//! x86 guest ACPI composed from the resolved VM device graph.

mod aml;
mod config;
mod fw_cfg;
mod serial;
mod tables;

pub(super) use config::X86FirmwarePlan;
pub(super) use fw_cfg::build_fw_cfg_blobs;
pub(super) use tables::build_direct_image;
