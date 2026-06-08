#![no_std]
#![no_main]

extern crate alloc;

#[cfg(not(target_os = "uefi"))]
compile_error!("axloader board builds require a *-unknown-uefi target and one board-* feature");

#[cfg(target_os = "uefi")]
mod boards;
#[cfg(target_os = "uefi")]
mod console;
#[cfg(target_os = "uefi")]
mod control;
#[cfg(target_os = "uefi")]
mod entry;
#[cfg(target_os = "uefi")]
mod http;
#[cfg(target_os = "uefi")]
mod identity;
#[cfg(target_os = "uefi")]
mod uefi_boot;

#[cfg(target_os = "uefi")]
use uefi::{Status, prelude::*};

#[cfg(target_os = "uefi")]
const MANIFEST_LIMIT: usize = 4096;
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
        if fetch_control_offer() || fetch_manifest() {
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
            logln!("elf_loader_pending: falling back to legacy manifest loader");
            false
        }
        Err(control::ControlError::NoServerUrl) => false,
        Err(err) => {
            logln!("control_boot_error: {err:?}");
            false
        }
    }
}

#[cfg(target_os = "uefi")]
fn fetch_manifest() -> bool {
    let mut manifest_url = [0u8; 1024];
    match uefi_boot::manifest_url_from_loaded_image(&mut manifest_url) {
        Ok(url) => {
            logln!("manifest_url: {url}");
            download_and_parse_manifest(url)
        }
        Err(err) => match boards::active::DEFAULT_MANIFEST_URL {
            Some(url) => {
                logln!("manifest_url_fallback: {url}");
                download_and_parse_manifest(url)
            }
            None => {
                logln!("manifest_url_error: {err:?}");
                false
            }
        },
    }
}

#[cfg(target_os = "uefi")]
fn download_and_parse_manifest(url: &str) -> bool {
    let body = match http::download_body(url, MANIFEST_LIMIT) {
        Ok(body) => body,
        Err(err) => {
            logln!(
                "manifest_download_error: {err:?} after {} attempts",
                http::retry_limit()
            );
            return false;
        }
    };

    match httpboot_protocol::parse_downloaded_manifest(&body, MANIFEST_LIMIT) {
        Ok(manifest) => {
            if manifest.arch != boards::active::ARCH_NAME {
                logln!(
                    "manifest_arch_error: expected={} got={}",
                    boards::active::ARCH_NAME,
                    manifest.arch
                );
                return false;
            }
            logln!(
                "manifest: arch={} load={:#x} entry={:#x} kernel_size={}",
                manifest.arch,
                manifest.kernel_load_addr,
                manifest.entry_point,
                manifest.kernel_size
            );
            logln!("kernel_url: {}", manifest.kernel_url);
            match http::download_kernel(
                manifest.kernel_url,
                manifest.kernel_load_addr,
                manifest.kernel_size,
            ) {
                Ok(kernel) => {
                    logln!(
                        "kernel_loaded: ptr={:p} pages={} size={}",
                        kernel.ptr.as_ptr(),
                        kernel.page_count,
                        kernel.size
                    );
                    match entry::exit_boot_services_and_jump(manifest.entry_point) {
                        Ok(()) => logln!("jump_error: entry returned unexpectedly"),
                        Err(err) => logln!("jump_error: {err:?}"),
                    }
                    false
                }
                Err(err) => {
                    logln!(
                        "kernel_download_error: {err:?} after {} attempts",
                        http::retry_limit()
                    );
                    false
                }
            }
        }
        Err(err) => {
            logln!("manifest_parse_error: {err:?}");
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
