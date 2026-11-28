#![no_std]

#[cfg(test)]
extern crate alloc;

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod address;
pub mod id;
pub mod memory;
mod staticcell;

pub use staticcell::StaticCell;
