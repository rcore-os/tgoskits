mod arch;
pub mod boot_server;
pub mod console;
#[cfg(target_arch = "x86_64")]
pub mod direct;
pub mod elf_loader;
mod http;
#[cfg(target_arch = "x86_64")]
pub mod network;
pub mod serial;
pub mod smbios;

#[cfg(target_arch = "riscv64")]
mod fdt;
#[cfg(target_arch = "riscv64")]
mod identity;
#[cfg(target_arch = "riscv64")]
pub mod snp;

use core::time::Duration;

use uefi::{Status, boot, prelude::*};

use crate::logln;

#[entry]
fn efi_main() -> Status {
    if let Err(error) = uefi::helpers::init() {
        return error.status();
    }
    arch::run()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    logln!("panic: {info}");
    loop {
        boot::stall(Duration::from_secs(1));
    }
}
