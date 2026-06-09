#![cfg_attr(target_os = "uefi", no_std)]
#![cfg_attr(target_os = "uefi", no_main)]

#[cfg(target_os = "uefi")]
extern crate alloc;

#[cfg(not(target_os = "uefi"))]
fn main() {}

#[cfg(target_os = "uefi")]
mod boards;
#[cfg(target_os = "uefi")]
mod console;
#[cfg(target_os = "uefi")]
mod control;
#[cfg(target_os = "uefi")]
mod discovery;
#[cfg(target_os = "uefi")]
mod elf_loader;
#[cfg(target_os = "uefi")]
mod entry;
#[cfg(target_os = "uefi")]
mod http;
#[cfg(target_os = "uefi")]
mod identity;

#[cfg(target_os = "uefi")]
use uefi::{Status, prelude::*};

#[cfg(target_os = "uefi")]
const BOOT_ROUND_RETRY_LIMIT: usize = 10;
#[cfg(target_os = "uefi")]
const BOOT_ROUND_RETRY_STALL: core::time::Duration = core::time::Duration::from_secs(3);

#[cfg(target_os = "uefi")]
#[entry]
fn efi_main() -> Status {
    uefi::helpers::init().expect("failed to initialize UEFI helpers");
    for round in 1..=BOOT_ROUND_RETRY_LIMIT {
        logln!("HTTP bootloader");
        logln!("round: {round}/{BOOT_ROUND_RETRY_LIMIT}");
        logln!("board: {}", boards::active::BOARD_NAME);
        logln!("arch: {}", boards::active::ARCH_NAME);
        logln!("output: {}", boards::active::OUTPUT_FILE);
        if fetch_control_offer() {
            return Status::SUCCESS;
        }
        if round < BOOT_ROUND_RETRY_LIMIT {
            logln!("boot_retry_wait: {} ms", BOOT_ROUND_RETRY_STALL.as_millis());
            uefi::boot::stall(BOOT_ROUND_RETRY_STALL);
        }
    }
    logln!("error: HTTP Boot retry limit reached");
    Status::SUCCESS
}

#[cfg(target_os = "uefi")]
fn fetch_control_offer() -> bool {
    match control::fetch_boot_offer() {
        Ok(offer) => {
            logln!(
                "boot_offer: boot_id={} arch={} format={} kernel_size={}",
                offer.boot_id,
                offer.arch,
                offer.image_format,
                offer.kernel_size
            );
            logln!("kernel_url: {}", offer.kernel_url);
            if let Some(entry_symbol) = offer.entry_symbol.as_deref() {
                logln!("entry_symbol: {entry_symbol}");
            }
            match elf_loader::download_and_load(
                &offer.kernel_url,
                offer.kernel_size,
                offer.entry_symbol.as_deref(),
            ) {
                Ok(elf) => {
                    logln!(
                        "elf_loaded: load={:#x} end={:#x} pages={} entry={:#x}",
                        elf.load_addr,
                        elf.load_end,
                        elf.page_count,
                        elf.entry_point
                    );
                    match entry::exit_boot_services_and_jump(elf.entry_point) {
                        Ok(()) => logln!("jump_error: entry returned unexpectedly"),
                        Err(err) => logln!("jump_error: {err:?}"),
                    }
                    false
                }
                Err(err) => {
                    logln!("elf_load_error: {err:?}");
                    false
                }
            }
        }
        Err(control::ControlError::NoServerUrl) => false,
        Err(err) => {
            logln!("control_boot_error: {err:?}");
            false
        }
    }
}

#[cfg(target_os = "uefi")]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    logln!("panic: {info}");
    loop {
        core::hint::spin_loop();
    }
}
