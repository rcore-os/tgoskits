pub mod console;
pub mod control;
pub mod elf_loader;
pub mod entry;
pub mod http;
pub mod network;
pub mod smbios;

use httpboot_protocol::LoaderStatusPhase;
use uefi::{Status, prelude::*};

use crate::logln;

#[cfg(target_arch = "x86_64")]
const TARGET_ARCH_NAME: &str = "x86_64";
#[cfg(target_arch = "x86_64")]
const EFI_OUTPUT_FILE: &str = "BOOTX64.EFI";

const MAX_DISCOVERY_BACKOFF_SECS: u64 = 10;

#[entry]
fn efi_main() -> Status {
    uefi::helpers::init().expect("failed to initialize UEFI helpers");
    let mut failed_boot_id = None;
    let mut discovery_backoff_secs = 1_u64;
    loop {
        logln!("HTTP bootloader");
        logln!("arch: {TARGET_ARCH_NAME}");
        logln!("output: {EFI_OUTPUT_FILE}");
        match fetch_control_offer(failed_boot_id.as_deref()) {
            BootAttempt::HandoffReturned => return Status::LOAD_ERROR,
            BootAttempt::Failed(boot_id) => {
                failed_boot_id = boot_id;
                discovery_backoff_secs = 1;
            }
            BootAttempt::DiscoveryFailed => {
                logln!("discovery_retry_wait: {discovery_backoff_secs} s");
                uefi::boot::stall(core::time::Duration::from_secs(discovery_backoff_secs));
                discovery_backoff_secs =
                    (discovery_backoff_secs * 2).min(MAX_DISCOVERY_BACKOFF_SECS);
            }
        }
    }
}

enum BootAttempt {
    DiscoveryFailed,
    Failed(Option<alloc::string::String>),
    HandoffReturned,
}

fn fetch_control_offer(failed_boot_id: Option<&str>) -> BootAttempt {
    match control::fetch_boot_offer(failed_boot_id) {
        Ok(network_boot) => {
            let offer = &network_boot.offer;
            logln!(
                "boot_offer: board_id={} session_id={} boot_id={} arch={:?} format={:?} \
                 kernel_size={}",
                offer.board_id,
                offer.session_id,
                offer.boot_id,
                offer.arch,
                offer.image_format,
                offer.kernel_size
            );
            if let Some(entry_symbol) = offer.entry_symbol.as_deref() {
                logln!("entry_symbol: {entry_symbol}");
            }
            if let Err(err) = network_boot.report_status(LoaderStatusPhase::Downloading {
                received: 0,
                total: offer.kernel_size,
            }) {
                logln!("loader_status_error: {err:?}");
                return BootAttempt::DiscoveryFailed;
            }
            match elf_loader::download_and_load(
                network_boot.interface.handle(),
                &offer.kernel_url,
                offer.kernel_size,
                &offer.kernel_sha256,
                offer.entry_symbol.as_deref(),
            ) {
                Ok(elf) => {
                    if let Err(err) = network_boot.report_status(LoaderStatusPhase::Verified) {
                        logln!("loader_status_error: {err:?}");
                        return BootAttempt::DiscoveryFailed;
                    }
                    logln!(
                        "elf_loaded: load={:#x} end={:#x} pages={} entry={:#x} handoff={:?}",
                        elf.load_addr,
                        elf.load_end,
                        elf.page_count,
                        elf.entry_point,
                        elf.handoff
                    );
                    if let Err(err) = network_boot.report_status(LoaderStatusPhase::ReadyToHandoff)
                    {
                        logln!("loader_status_error: {err:?}");
                        return BootAttempt::DiscoveryFailed;
                    }
                    let entry_point = elf.entry_point;
                    let handoff = elf.handoff;
                    drop(network_boot);
                    let jump_result = match handoff {
                        elf_loader::EntryHandoff::BootInfo => {
                            entry::exit_boot_services_and_jump(entry_point)
                        }
                        elf_loader::EntryHandoff::Uefi => entry::jump_to_uefi_entry(entry_point),
                    };
                    match jump_result {
                        Ok(()) => logln!("jump_error: entry returned unexpectedly"),
                        Err(err) => logln!("jump_error: {err:?}"),
                    }
                    BootAttempt::HandoffReturned
                }
                Err(err) => {
                    logln!("elf_load_error: {err:?}");
                    let _ = network_boot.report_status(LoaderStatusPhase::Failed {
                        code: "kernel_load_failed".into(),
                        message: alloc::format!("{err:?}"),
                    });
                    BootAttempt::Failed(Some(offer.boot_id.clone()))
                }
            }
        }
        Err(err) => {
            logln!("control_boot_error: {err:?}");
            BootAttempt::DiscoveryFailed
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
