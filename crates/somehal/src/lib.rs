#![no_std]
#![no_main]
#![feature(iter_next_chunk)]

#[macro_use]
extern crate alloc;

#[macro_use]
extern crate log;

#[macro_use]
pub mod console;

#[cfg(target_arch = "loongarch64")]
#[path = "arch/loongarch64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "aarch64")]
#[path = "arch/aarch64/mod.rs"]
pub mod arch;

mod acpi;
mod cmdline;
mod consts;
#[cfg(efi)]
mod efi_stub;
mod elf;
pub(crate) mod fdt;
pub mod mem;

pub use somehal_macros::{entry, secondary_entry};

trait ArchTrait {
    fn kernel_code() -> &'static [u8];
    fn post_allocator();

    fn _pa(vaddr: *const u8) -> usize;
    fn _va(paddr: usize) -> *mut u8;
    fn _io(paddr: usize) -> *mut u8;
    fn ioremap(paddr: usize, size: usize) -> *mut u8;
}

pub fn post_allocator() {
    debug!("alloc ok");
    arch::Arch::post_allocator();
}

fn kernel_code() -> &'static [u8] {
    arch::Arch::kernel_code()
}

fn prime_entry() -> ! {
    mem::set_mmu_enabled();
    fdt::setup_earlycon();
    fdt::setup_memory_map();
    acpi::earlycon::acpi_setup_earlycon().unwrap();

    mem::print_memory_map();

    unsafe extern "C" {
        fn __somehal_main() -> !;
    }
    unsafe { __somehal_main() }
}
