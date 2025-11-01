#![no_std]
#![no_main]

#[macro_use]
extern crate alloc;

#[macro_use]
mod console;

#[cfg(target_arch = "loongarch64")]
#[path = "arch/loongarch64/mod.rs"]
pub mod arch;

mod acpi;
#[cfg(efi)]
mod efi_stub;
mod elf;

mod mem;
