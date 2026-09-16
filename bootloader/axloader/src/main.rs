#![cfg_attr(target_os = "uefi", no_std)]
#![cfg_attr(target_os = "uefi", no_main)]

#[cfg(target_os = "uefi")]
extern crate alloc;

#[cfg(not(target_os = "uefi"))]
mod signing;

#[cfg(not(target_os = "uefi"))]
fn main() -> anyhow::Result<()> {
    signing::run()
}

#[cfg(target_os = "uefi")]
mod loader;

#[cfg(target_os = "uefi")]
mod uefi_runtime;

#[cfg(target_os = "uefi")]
pub use loader::{console, control, elf_loader, entry, http};
