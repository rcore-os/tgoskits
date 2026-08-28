//! Architecture-neutral ACPI image layout and QEMU loader composition.

mod arena;
mod device;
mod error;
#[cfg(any(target_arch = "x86_64", test))]
mod image;
mod loader;

#[cfg(any(target_arch = "x86_64", test))]
pub(crate) use arena::AcpiAllocation;
pub(crate) use arena::AcpiTableArena;
#[cfg(target_arch = "x86_64")]
pub(crate) use device::ResolvedAcpiInterrupt;
pub(crate) use device::{
    AcpiInterruptControllerMap, ResolvedAcpiDevice, ResolvedAcpiProperty, ResolvedAcpiRegister,
    ResolvedAcpiSpecial, ResolvedAcpiSpecialKind, encode_devices_with_interrupt_controllers,
    resolve_acpi_firmware,
};
pub use error::AcpiBuildError;
#[cfg(any(target_arch = "x86_64", test))]
pub(crate) use image::{AcpiImage, AcpiTableRecord, AcpiTableSet};
pub(crate) use loader::{AcpiLoaderPlan, LoaderZone};
