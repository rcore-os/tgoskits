pub mod console;
pub mod control;
pub mod elf_loader;
pub mod entry;
pub mod http;

use uefi::{Status, prelude::*};

use crate::logln;

#[cfg(target_arch = "x86_64")]
const TARGET_ARCH_NAME: &str = "x86_64";
#[cfg(target_arch = "x86_64")]
const EFI_OUTPUT_FILE: &str = "BOOTX64.EFI";

const BOOT_ROUND_RETRY_LIMIT: usize = 10;
const BOOT_ROUND_RETRY_STALL: core::time::Duration = core::time::Duration::from_secs(3);

#[entry]
fn efi_main() -> Status {
    uefi::helpers::init().expect("failed to initialize UEFI helpers");
    for round in 1..=BOOT_ROUND_RETRY_LIMIT {
        logln!("HTTP bootloader");
        logln!("round: {round}/{BOOT_ROUND_RETRY_LIMIT}");
        logln!("arch: {TARGET_ARCH_NAME}");
        logln!("output: {EFI_OUTPUT_FILE}");
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
                        "elf_loaded: load={:#x} end={:#x} pages={} entry={:#x} handoff={:?}",
                        elf.load_addr,
                        elf.load_end,
                        elf.page_count,
                        elf.entry_point,
                        elf.handoff
                    );
                    let jump_result = match elf.handoff {
                        elf_loader::EntryHandoff::BootInfo => {
                            entry::exit_boot_services_and_jump(elf.entry_point)
                        }
                        elf_loader::EntryHandoff::Uefi => {
                            entry::jump_to_uefi_entry(elf.entry_point)
                        }
                    };
                    match jump_result {
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
        Err(err) => {
            logln!("control_boot_error: {err:?}");
            false
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    logln!("panic: {info}");
    loop {
        core::hint::spin_loop();
    }
}
