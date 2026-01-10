use core::{fmt::Write, ptr::null, sync::atomic::AtomicBool};

use uefi::{
    Result, boot,
    mem::memory_map::MemoryMap,
    prelude::*,
    proto::loaded_image::LoadedImage,
    system::with_config_table,
    table::{self, cfg::ConfigTableEntry},
};

use crate::{acpi::set_rsdp, arch::relocate};

pub(crate) mod memmap;
pub mod pe;

/// EFI PE 入口点 - 符合 EFI ABI 的汇编包装
/// 参数: a0 = image_handle, a1 = system_table
#[unsafe(export_name = "efi_pe_entry")]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn efi_pe_entry(
    image_handle: Handle,
    system_table: *const ::core::ffi::c_void,
) -> Status {
    unsafe {
        relocate();
        boot::set_image_handle(image_handle);
        table::set_system_table(system_table.cast());

        crate::console::set_out(&UefiPrinter);
        println!("UEFI application started.");

        if let Err(e) = efi_main() {
            println!("EFI application error: {:?}", e);
            return e.status();
        }

        let _ = crate::acpi::earlycon::acpi_setup_earlycon();

        UEFI_SERVICE_OK.store(false, core::sync::atomic::Ordering::Relaxed);
        let mem_map = boot::exit_boot_services(None);

        println!("Exited boot services, owned memory map obtained.");
        memmap::setup_memory_map(mem_map.entries()).unwrap();

        crate::arch::entry::kernel_entry(1, null(), system_table);
        unreachable!()
    }
}

fn efi_main() -> Result {
    find_acpi_rsdp();

    println!("Page size: {:#x} bytes", crate::mem::page_size());

    let h = boot::get_handle_for_protocol::<LoadedImage>()?;

    let img = boot::open_protocol_exclusive::<LoadedImage>(h)?;

    match img.load_options_as_cstr16() {
        Ok(cmdline) => {
            println!("Kernel command line: {}", cmdline);
            uefi::system::with_stdout(|stdout| {
                let _ = cmdline.as_str_in_buf(stdout);
            });
        }
        Err(e) => {
            println!("Failed to get load options as CStr16: {:?}", e);
        }
    }

    Ok(())
}

static UEFI_SERVICE_OK: AtomicBool = AtomicBool::new(true);

struct UefiPrinter;
impl crate::console::Con for UefiPrinter {
    fn write_str(&self, s: &str) {
        if !UEFI_SERVICE_OK.load(core::sync::atomic::Ordering::Relaxed) {
            return;
        }
        uefi::system::with_stdout(|stdout| {
            let _ = stdout.write_str(s);
        });
    }
}

fn find_acpi_rsdp() {
    with_config_table(|config_table| {
        let mut version = 0;
        let mut addr = null();

        for entry in config_table {
            if entry.guid == ConfigTableEntry::ACPI2_GUID {
                // ACPI 2.0 RSDP (推荐)
                println!("Found ACPI 2.0 RSDP at address: {:p}", entry.address);
                version = 2;
                addr = entry.address;
                break;
            }

            if entry.guid == ConfigTableEntry::ACPI_GUID {
                // ACPI 1.0 RSDP (备选)
                println!("Found ACPI 1.0 RSDP at address: {:p}", entry.address);
                if version == 0 {
                    version = 1;
                    addr = entry.address;
                }
            }
        }

        if !addr.is_null() {
            println!("Using ACPI {} RSDP at address: {:p}", version, addr);
            set_rsdp(addr);
        } else {
            println!("No ACPI RSDP found in UEFI config tables.");
        }
    })
}
