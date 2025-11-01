use core::{fmt::Write, ptr::null};

use acpi::sdt::{madt::Madt, spcr::Spcr};
use uefi::{
    Result,
    boot::{MemoryDescriptor, MemoryType},
    mem::memory_map::MemoryMap,
    prelude::*,
    proto::{console::gop::GraphicsOutput, loaded_image::LoadedImage},
    system::with_config_table,
    table::cfg::{ACPI_GUID, ACPI2_GUID},
};
use uefi_raw::table::system::SystemTable;

use crate::{
    acpi::{dbg2::Dbg2, set_rsdp},
    arch::relocate,
    efi_stub::acpi_handle::AcpiHandle,
    mem::{self, MB, page_size},
};

mod acpi_handle;
pub mod pe;

/// EFI PE 入口点 - 符合 EFI ABI 的汇编包装
/// 参数: a0 = image_handle, a1 = system_table
#[unsafe(export_name = "efi_pe_entry")]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn efi_pe_entry(
    image_handle: Handle,
    system_table: *const SystemTable,
) -> Status {
    unsafe {
        relocate();
        ::uefi::boot::set_image_handle(image_handle);
        ::uefi::table::set_system_table(system_table);

        crate::console::set_printer(&UefiPrinter);

        if let Err(e) = efi_main() {
            println!("EFI application error: {:?}", e);
            return e.status();
        }

        if let Err(e) = draw_sierpinski() {
            println!("Failed to draw Sierpinski triangle: {:?}", e);
        } else {
            println!("Sierpinski triangle drawn successfully.");
        }

        crate::arch::entry::efi_kernel_prepare();
    }

    // 返回成功状态
    Status::SUCCESS
}

fn efi_main() -> Result {
    find_acpi_rsdp();

    println!("Page size: {:#x} bytes", crate::mem::page_size());

    let mem_map = boot::memory_map(MemoryType::LOADER_DATA)?;
    for desc in mem_map.entries() {
        if matches!(desc.ty, MemoryType::CONVENTIONAL)
            && desc.page_count as usize >= 2 * MB / page_size()
        {
            println!("{desc:#x?}");
            mem::add_memory_descriptor(desc.into());
        }
    }

    find_debug();

    let h = boot::get_handle_for_protocol::<LoadedImage>()?;

    let img = boot::open_protocol_exclusive::<LoadedImage>(h)?;

    match img.load_options_as_cstr16() {
        Ok(cmdline) => {
            println!("Kernel command line: {}", cmdline);
            system::with_stdout(|stdout| {
                let _ = cmdline.as_str_in_buf(stdout);
            });
        }
        Err(e) => {
            println!("Failed to get load options as CStr16: {:?}", e);
        }
    }

    Ok(())
}

fn draw_sierpinski() -> Result {
    // Open graphics output protocol.
    let gop_handle = boot::get_handle_for_protocol::<GraphicsOutput>()?;
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle)?;
    Ok(())
}

struct UefiPrinter;
impl crate::console::Printer for UefiPrinter {
    fn read_byte(&self) -> Option<u8> {
        // system::with_stdin(|stdin| {
        //     let mut buffer = [0u16; 1];
        //     match stdin.read_key(&mut buffer) {
        //         Ok(()) => Some(buffer[0] as u8),
        //         Err(_) => None,
        //     }
        // })
        None
    }

    fn write_str(&self, s: &str) {
        system::with_stdout(|stdout| {
            let _ = stdout.write_str(s);
        });
    }
}

fn find_acpi_rsdp() {
    with_config_table(|config_table| {
        let mut version = 0;
        let mut addr = null();

        for entry in config_table {
            if entry.guid == ACPI2_GUID {
                // ACPI 2.0 RSDP (推荐)
                println!("Found ACPI 2.0 RSDP at address: {:p}", entry.address);
                version = 2;
                addr = entry.address;
                break;
            }

            if entry.guid == ACPI_GUID {
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

impl From<&MemoryDescriptor> for crate::mem::MemoryDescriptor {
    fn from(value: &MemoryDescriptor) -> Self {
        crate::mem::MemoryDescriptor {
            physical_start: value.phys_start as usize,
            size_in_bytes: (value.page_count as usize) * page_size(),
        }
    }
}

fn find_debug() -> Option<()> {
    let tb = match crate::acpi::tables(AcpiHandle) {
        Ok(t) => t,
        Err(e) => {
            println!("Failed to get ACPI tables: {:?}", e);
            return None;
        }
    };

    for spsr in tb.find_tables::<Spcr>() {
        println!("Found SPCR table: {:#x?}", spsr);
    }

    Some(())
}
