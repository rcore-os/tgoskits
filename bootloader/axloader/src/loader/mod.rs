mod arch;
pub mod boot_server;
pub mod console;
pub mod direct;
pub mod elf_loader;
mod http;
mod identity;
pub mod network;
pub mod serial;
pub mod smbios;
mod tcp4;

#[cfg(target_arch = "riscv64")]
mod fdt;
#[cfg(target_arch = "riscv64")]
pub mod snp;

use core::time::Duration;

use uefi::{Status, boot, prelude::*};

use crate::logln;

const WATCHDOG_APPLICATION_CODE: u64 = 0x0001_0000;

#[entry]
fn efi_main() -> Status {
    if let Err(error) = uefi::helpers::init() {
        return error.status();
    }
    if let Err(error) = boot::set_watchdog_timer(0, WATCHDOG_APPLICATION_CODE, None) {
        let status = error.status();
        logln!("loader_watchdog_disable_failed: {status:?}");
        return status;
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
