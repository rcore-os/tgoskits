#![no_std]
#![no_main]

#[cfg(not(all(target_os = "uefi", feature = "board-asus-nuc15crh")))]
compile_error!(
    "bootloader-http currently requires a UEFI target and --features board-asus-nuc15crh"
);

#[cfg(all(target_os = "uefi", feature = "board-asus-nuc15crh"))]
mod boards;
#[cfg(all(target_os = "uefi", feature = "board-asus-nuc15crh"))]
mod uefi_boot;

#[cfg(all(target_os = "uefi", feature = "board-asus-nuc15crh"))]
use uefi::{Status, prelude::*};

#[cfg(all(target_os = "uefi", feature = "board-asus-nuc15crh"))]
#[entry]
fn efi_main() -> Status {
    uefi::helpers::init().expect("failed to initialize UEFI helpers");
    uefi::println!("HTTP bootloader");
    uefi::println!("board: {}", boards::active::BOARD_NAME);
    uefi::println!("arch: {}", boards::active::ARCH_NAME);
    uefi::println!("output: {}", boards::active::OUTPUT_FILE);
    print_manifest_url();
    smoke_manifest_parser();
    Status::SUCCESS
}

#[cfg(all(target_os = "uefi", feature = "board-asus-nuc15crh"))]
fn print_manifest_url() {
    let mut manifest_url = [0u8; 1024];
    match uefi_boot::manifest_url_from_loaded_image(&mut manifest_url) {
        Ok(url) => uefi::println!("manifest_url: {url}"),
        Err(err) => uefi::println!("manifest_url_error: {err:?}"),
    }
}

#[cfg(all(target_os = "uefi", feature = "board-asus-nuc15crh"))]
fn smoke_manifest_parser() {
    let manifest = br#"{
        "kernel_url": "http://127.0.0.1/kernel.bin",
        "kernel_size": 1,
        "kernel_load_addr": "0x200000",
        "entry_point": "0x200000",
        "arch": "x86_64"
    }"#;
    match bootloader_common::parse_downloaded_manifest(manifest, 512) {
        Ok(parsed) => uefi::println!(
            "manifest: arch={} load={:#x} entry={:#x}",
            parsed.arch,
            parsed.kernel_load_addr,
            parsed.entry_point
        ),
        Err(err) => uefi::println!("manifest_error: {err:?}"),
    }
}

#[cfg(all(target_os = "uefi", feature = "board-asus-nuc15crh"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    uefi::println!("panic: {info}");
    loop {
        core::hint::spin_loop();
    }
}
