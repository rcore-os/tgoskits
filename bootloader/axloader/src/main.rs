#![cfg_attr(target_os = "uefi", no_std)]
#![cfg_attr(target_os = "uefi", no_main)]

#[cfg(target_os = "uefi")]
extern crate alloc;

#[cfg(target_os = "uefi")]
mod control_input;

#[cfg(not(target_os = "uefi"))]
fn main() {}

#[cfg(target_os = "uefi")]
mod loader;

#[cfg(target_os = "uefi")]
mod uefi_runtime;

#[cfg(target_os = "uefi")]
pub use loader::{console, control, elf_loader, entry, http};
