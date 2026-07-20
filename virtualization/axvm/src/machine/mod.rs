//! Architecture-neutral VM machine planning.
//!
//! The planner converts an immutable VM request and a host-platform snapshot
//! into one deterministic description consumed by address-space, interrupt,
//! device, and firmware construction.

mod acpi_firmware;
mod controller;
mod error;
mod fdt;
mod firmware;
mod host;
mod host_fdt;
mod loongarch;
mod planner;
mod request;
mod riscv_firmware;
mod transaction;
mod types;

pub use acpi_firmware::*;
pub use axdevice::{
    ConsoleRxPolicy, ConsoleTxPolicy, ControllerInputId, DeviceBackend, DeviceModelId,
    DeviceRequirement, DeviceRequirements, HostConsoleBackend, ResolvedDeviceResources,
    ResourceSlot,
};
pub use controller::*;
pub use error::*;
pub use fdt::*;
pub use firmware::*;
pub use host::*;
pub use host_fdt::*;
pub use loongarch::*;
pub use planner::*;
pub use request::*;
pub use riscv_firmware::*;
pub use transaction::*;
pub use types::*;
