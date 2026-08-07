//! Architecture-neutral ACPI image layout and QEMU loader composition.

mod arena;
mod error;
#[cfg(any(target_arch = "x86_64", test))]
mod image;
mod loader;

#[cfg(any(target_arch = "x86_64", test))]
pub(crate) use arena::AcpiAllocation;
pub(crate) use arena::AcpiTableArena;
pub use error::AcpiBuildError;
#[cfg(any(target_arch = "x86_64", test))]
pub(crate) use image::{AcpiImage, AcpiTableRecord, AcpiTableSet};
pub(crate) use loader::{AcpiLoaderPlan, LoaderZone};
