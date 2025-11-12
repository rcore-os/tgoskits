#![no_std]
#![no_main]

#[macro_use]
extern crate alloc;

#[macro_use]
mod console;

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
mod mem;

trait ArchTrait {
    fn kernel_code() -> &'static [u8];
    fn post_allocator();
}

pub fn post_allocator() {
    arch::Arch::post_allocator();
}

fn kernel_code() -> &'static [u8] {
    arch::Arch::kernel_code()
}

fn prime_entry() -> ! {
    mem::set_mmu_enabled();
    fdt::setup_earlycon();
    fdt::setup_memory_map();

    println!("All tests passed!");
    loop {}
}
