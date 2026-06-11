#[cfg(target_arch = "x86_64")]
use core::arch::naked_asm;
use core::{
    ffi::c_void,
    fmt::Write,
    mem::MaybeUninit,
    ptr::{addr_of_mut, null},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub use uefi::Status;
#[cfg(target_arch = "loongarch64")]
pub use uefi::runtime::ResetType;
use uefi::{
    Result,
    boot::{self, MemoryDescriptor, MemoryType},
    prelude::*,
    proto::loaded_image::LoadedImage,
    runtime::{self, set_virtual_address_map},
    system::with_config_table,
    table::{self, cfg::ConfigTableEntry},
};

use crate::{
    ArchTrait,
    acpi::set_rsdp,
    arch::{Arch, relocate},
    mem::{__io, __va},
};

const EFI_IMAGE_HANDLE_UNSET: usize = usize::MAX;
const EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE: usize = 128 * 1024;
const EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY: usize = 1024;
const EXIT_BOOT_MEMORY_MAP_RETRIES: usize = 3;

#[repr(align(8))]
struct AlignedBytes<const N: usize>([u8; N]);

static mut EXIT_BOOT_MEMORY_MAP_BUFFER: AlignedBytes<EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE> =
    AlignedBytes([0; EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE]);
static mut EXIT_BOOT_MEMORY_MAP_DESCRIPTORS: [MaybeUninit<MemoryDescriptor>;
    EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY] =
    [const { MaybeUninit::uninit() }; EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY];
static EFI_IMAGE_HANDLE: AtomicUsize = AtomicUsize::new(EFI_IMAGE_HANDLE_UNSET);

pub(crate) fn setup_service(system_table: *const ::core::ffi::c_void) {
    unsafe { table::set_system_table(system_table.cast()) };
    if let Some(image_handle) = saved_image_handle() {
        unsafe { boot::set_image_handle(image_handle) };
    }
    setup_console();
    println!("UEFI console ok.");
    find_acpi_rsdp();
}

pub(crate) mod memmap;
pub mod pe;

/// EFI PE 入口点 - 符合 EFI ABI 的汇编包装
/// 参数: a0 = image_handle, a1 = system_table
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text")]
pub unsafe extern "C" fn __x86_64_efi_pe_entry() -> Status {
    naked_asm!(
        "sub rsp, 8",
        "mov r12, rcx",
        "mov r13, rdx",
        "call {relocate}",
        "mov rdi, r12",
        "mov rsi, r13",
        "add rsp, 8",
        "jmp {entry}",
        relocate = sym relocate,
        entry = sym efi_pe_entry_main,
    )
}

unsafe extern "C" fn efi_pe_entry_main(
    image_handle: Handle,
    system_table: *const ::core::ffi::c_void,
) -> Status {
    unsafe {
        save_image_handle(image_handle);
        boot::set_image_handle(image_handle);
        table::set_system_table(system_table.cast());
        setup_console();
        println!("UEFI application started.");
        // Safety: `system_table` comes from the EFI firmware entry path and
        // matches the contract documented on `ArchTrait::efi_enter_kernel`.
        if Arch::efi_enter_kernel(system_table) {
            Status::SUCCESS
        } else {
            unreachable!()
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
#[unsafe(export_name = "efi_pe_entry")]
#[unsafe(link_section = ".text")]
pub unsafe extern "efiapi" fn efi_pe_entry(
    image_handle: Handle,
    system_table: *const ::core::ffi::c_void,
) -> Status {
    unsafe {
        relocate();
        efi_pe_entry_main(image_handle, system_table)
    }
}

pub(crate) fn exit_boot_services() {
    println!("Exiting UEFI boot services...");
    UEFI_SERVICE_EXIT.store(true, core::sync::atomic::Ordering::Relaxed);
    let mem_map = unsafe { exit_boot_services_no_alloc() };
    println!("Exited boot services, memory map obtained.");

    let mut new_map: heapless::Vec<MemoryDescriptor, 32> = heapless::Vec::new();

    for entry in mem_map.entries() {
        match entry.ty {
            MemoryType::RUNTIME_SERVICES_CODE | MemoryType::RUNTIME_SERVICES_DATA => {
                let mut en = *entry;
                en.virt_start = __va(entry.phys_start as _) as usize as _;
                new_map.push(en).unwrap();
            }
            MemoryType::MMIO => {
                let mut en = *entry;
                en.virt_start = __io(entry.phys_start as _) as usize as _;
                new_map.push(en).unwrap();
            }
            _ => {}
        }
    }

    unsafe {
        if let Some(st) = uefi::table::system_table_raw() {
            set_virtual_address_map(&mut new_map, __va(st.as_ptr() as _) as _)
                .expect("Failed to set virtual address map");
        }
    }

    memmap::setup_memory_map(mem_map.entries());
}

struct ExitBootMemoryMap {
    entries: &'static [MemoryDescriptor],
}

impl ExitBootMemoryMap {
    fn entries(&self) -> core::slice::Iter<'static, MemoryDescriptor> {
        self.entries.iter()
    }
}

unsafe fn exit_boot_services_no_alloc() -> ExitBootMemoryMap {
    let Some(system_table) = uefi::table::system_table_raw() else {
        reset_on_exit_boot_services_failure(Status::INVALID_PARAMETER);
    };
    let boot_services = unsafe {
        system_table
            .as_ref()
            .boot_services
            .as_ref()
            .unwrap_or_else(|| reset_on_exit_boot_services_failure(Status::INVALID_PARAMETER))
    };
    let Some(image_handle) = saved_image_handle() else {
        reset_on_exit_boot_services_failure(Status::INVALID_PARAMETER);
    };

    let mut status = Status::SUCCESS;

    for _ in 0..EXIT_BOOT_MEMORY_MAP_RETRIES {
        let mut map_size = EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE;
        let mut map_key = 0;
        let mut desc_size = 0;
        let mut desc_version = 0;
        let map_ptr =
            unsafe { addr_of_mut!(EXIT_BOOT_MEMORY_MAP_BUFFER.0).cast::<MemoryDescriptor>() };

        status = unsafe {
            (boot_services.get_memory_map)(
                &mut map_size,
                map_ptr,
                &mut map_key,
                &mut desc_size,
                &mut desc_version,
            )
        };
        if status == Status::BUFFER_TOO_SMALL {
            continue;
        }
        if status != Status::SUCCESS {
            reset_on_exit_boot_services_failure(status);
        }

        status = unsafe { (boot_services.exit_boot_services)(image_handle.as_ptr(), map_key) };
        if status == Status::SUCCESS {
            let entries =
                unsafe { copy_exit_boot_memory_map(map_ptr.cast_const(), map_size, desc_size) };
            return ExitBootMemoryMap { entries };
        }
    }

    reset_on_exit_boot_services_failure(status);
}

unsafe fn copy_exit_boot_memory_map(
    src: *const MemoryDescriptor,
    map_size: usize,
    desc_size: usize,
) -> &'static [MemoryDescriptor] {
    assert!(
        desc_size >= size_of::<MemoryDescriptor>(),
        "UEFI memory descriptor size is too small"
    );

    let entry_count = map_size / desc_size;
    assert!(
        entry_count <= EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY,
        "UEFI memory map has too many entries"
    );

    let dst =
        addr_of_mut!(EXIT_BOOT_MEMORY_MAP_DESCRIPTORS).cast::<MaybeUninit<MemoryDescriptor>>();
    for index in 0..entry_count {
        let entry = unsafe {
            src.cast::<u8>()
                .add(index * desc_size)
                .cast::<MemoryDescriptor>()
        };
        unsafe { dst.add(index).write(MaybeUninit::new(entry.read())) };
    }

    unsafe { core::slice::from_raw_parts(dst.cast::<MemoryDescriptor>(), entry_count) }
}

fn save_image_handle(image_handle: Handle) {
    EFI_IMAGE_HANDLE.store(image_handle.as_ptr() as usize, Ordering::Relaxed);
}

fn saved_image_handle() -> Option<Handle> {
    let raw = EFI_IMAGE_HANDLE.load(Ordering::Relaxed);
    if raw == EFI_IMAGE_HANDLE_UNSET {
        return None;
    }

    unsafe { Handle::from_ptr(raw as *mut c_void) }
}

fn reset_on_exit_boot_services_failure(status: Status) -> ! {
    unsafe {
        if let Some(system_table) = uefi::table::system_table_raw()
            && let Some(runtime_services) = system_table.as_ref().runtime_services.as_ref()
        {
            (runtime_services.reset_system)(runtime::ResetType::COLD, status, 0, null());
        }
    }

    loop {
        core::hint::spin_loop();
    }
}

pub(crate) fn setup_console() {
    unsafe { crate::console::set_out(&UefiPrinter) };
}

#[allow(dead_code)]
fn efi_main() -> Result {
    find_acpi_rsdp();

    println!("Page size: {:#x} bytes", crate::mem::page_size());

    let h = boot::get_handle_for_protocol::<LoadedImage>()?;

    let img = boot::open_protocol_exclusive::<LoadedImage>(h)?;

    match img.load_options_as_cstr16() {
        Ok(cmdline) => {
            println!("Kernel command line: {}", cmdline);
        }
        Err(e) => {
            println!("Failed to get load options as CStr16: {:?}", e);
        }
    }

    Ok(())
}

static UEFI_SERVICE_EXIT: AtomicBool = AtomicBool::new(false);

struct UefiPrinter;
impl crate::console::Con for UefiPrinter {
    fn write_str(&self, s: &str) {
        if UEFI_SERVICE_EXIT.load(core::sync::atomic::Ordering::Relaxed) {
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

#[cfg(target_arch = "loongarch64")]
pub fn is_uefi_available() -> bool {
    uefi::table::system_table_raw().is_some()
}

#[cfg(target_arch = "loongarch64")]
pub fn reset(reset_type: ResetType, status: Status, data: Option<&[u8]>) -> ! {
    info!("Resetting system via UEFI...");
    uefi::runtime::reset(reset_type, status, data)
}
