#[cfg(target_arch = "x86_64")]
use core::arch::naked_asm;
use core::{
    ffi::c_void,
    fmt::Write,
    mem::MaybeUninit,
    ptr::{addr_of_mut, null},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use host_boot_abi::{BOOT_PAYLOAD_GUID, BootPayload, MAX_CMDLINE};
pub use uefi::Status;
#[cfg(target_arch = "loongarch64")]
pub use uefi::runtime::ResetType;
use uefi::{
    Guid, Result,
    boot::{self, AllocateType, MemoryDescriptor, MemoryType},
    guid,
    prelude::*,
    proto::{
        loaded_image::LoadedImage,
        media::file::{File, FileAttribute, FileInfo, FileMode, FileType, RegularFile},
        rng::Rng,
    },
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
const FDT_TABLE_GUID: Guid = guid!("b1b621d5-f19c-41a5-830b-d9152c69aae0");

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
    find_fdt();
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
        load_efi_cmdline(image_handle);
        load_host_boot_payload();
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

    if let Some((start, end)) = crate::boot_payload::staged_uefi() {
        let allocation_end = end
            .checked_next_multiple_of(crate::consts::PAGE_SIZE)
            .expect("host archive page range overflows");
        assert!(
            mem_map.entries().any(|entry| {
                entry.ty == MemoryType::LOADER_DATA
                    && entry.phys_start <= start as u64
                    && entry
                        .page_count
                        .checked_mul(4096)
                        .and_then(|size| entry.phys_start.checked_add(size))
                        .is_some_and(|limit| allocation_end as u64 <= limit)
            }),
            "host initramfs is not backed by UEFI loader pages"
        );
    }

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
    crate::boot_payload::reserve_staged_uefi();
}

fn load_host_boot_payload() {
    let mut archive_staged = false;
    let installed = with_config_table(|tables| {
        tables
            .iter()
            .find(|entry| entry.guid == BOOT_PAYLOAD_GUID)
            .map(|entry| entry.address)
    });
    if let Some(address) = installed {
        assert!(!address.is_null(), "null host boot payload table");
        // SAFETY: UEFI owns this runtime-services configuration-table allocation
        // until ExitBootServices; the loader publishes a versioned BootPayload.
        let payload = unsafe { &*address.cast::<BootPayload>() };
        payload.validate().expect("invalid host boot payload");
        if payload.cmdline_len != 0 && !crate::cmdline::has_handoff_cmdline() {
            crate::cmdline::set_cmdline(payload.cmdline());
        }
        if payload.archive_len != 0 {
            let start =
                usize::try_from(payload.archive_start).expect("host archive address overflows");
            let len = usize::try_from(payload.archive_len).expect("host archive size overflows");
            let end = start
                .checked_add(len)
                .expect("host archive range overflows");
            crate::boot_payload::stage_uefi(start, end);
            archive_staged = true;
        }
    }

    if crate::cmdline::has_handoff_cmdline() && archive_staged {
        return;
    }

    let Ok(mut fs) = boot::get_image_file_system(boot::image_handle()) else {
        return;
    };
    let mut volume = fs.open_volume().expect("failed to open UEFI boot volume");
    if !crate::cmdline::has_handoff_cmdline()
        && let Ok(file) = volume.open(
            uefi::cstr16!("\\EFI\\BOOT\\cmdline.txt"),
            FileMode::Read,
            FileAttribute::empty(),
        )
    {
        let mut file = regular_file(file);
        let size = file_size(&mut file);
        assert!(size <= MAX_CMDLINE, "host command line is too long");
        let mut bytes = [0u8; MAX_CMDLINE];
        read_exact_file(&mut file, &mut bytes[..size]);
        let command = core::str::from_utf8(&bytes[..size]).expect("invalid host command line");
        assert!(!command.contains('\0'), "host command line contains NUL");
        crate::cmdline::set_cmdline(command.trim_end_matches(['\r', '\n']));
    }
    if !archive_staged
        && let Ok(file) = volume.open(
            uefi::cstr16!("\\EFI\\BOOT\\initramfs.cpio"),
            FileMode::Read,
            FileAttribute::empty(),
        )
    {
        let mut file = regular_file(file);
        let size = file_size(&mut file);
        assert!(
            size > 0 && size <= 1024 * 1024 * 1024,
            "invalid UEFI host archive size"
        );
        let pages = size.div_ceil(crate::consts::PAGE_SIZE);
        let address = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
            .expect("failed to allocate UEFI host archive");
        // SAFETY: allocate_pages returned at least `size` writable bytes.
        let bytes = unsafe { core::slice::from_raw_parts_mut(address.as_ptr(), size) };
        read_exact_file(&mut file, bytes);
        let start = address.as_ptr() as usize;
        crate::boot_payload::stage_uefi(
            start,
            start
                .checked_add(size)
                .expect("host archive range overflows"),
        );
        println!("UEFI host initramfs loaded: {size} bytes");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EfiCmdlineError {
    TooLong,
    InvalidCharacter,
}

fn decode_efi_cmdline<'a>(
    words: &[u16],
    bytes: &'a mut [u8; MAX_CMDLINE],
) -> core::result::Result<&'a str, EfiCmdlineError> {
    if words.len() >= bytes.len() {
        return Err(EfiCmdlineError::TooLong);
    }
    for (index, word) in words.iter().copied().enumerate() {
        let byte = u8::try_from(word).map_err(|_| EfiCmdlineError::InvalidCharacter)?;
        if byte != b' ' && !byte.is_ascii_graphic() {
            return Err(EfiCmdlineError::InvalidCharacter);
        }
        bytes[index] = byte;
    }
    core::str::from_utf8(&bytes[..words.len()]).map_err(|_| EfiCmdlineError::InvalidCharacter)
}

fn load_efi_cmdline(image_handle: Handle) {
    let image = boot::open_protocol_exclusive::<LoadedImage>(image_handle)
        .expect("failed to open LoadedImage for EFI command line");
    let options = match image.load_options_as_cstr16() {
        Ok(options) => options,
        Err(uefi::proto::loaded_image::LoadOptionsError::NotSet) => return,
        Err(error) => panic!("invalid EFI command line: {error:?}"),
    };
    let mut bytes = [0_u8; MAX_CMDLINE];
    match decode_efi_cmdline(options.to_u16_slice(), &mut bytes) {
        Ok("") => {}
        Ok(cmdline) => crate::cmdline::set_cmdline(cmdline),
        Err(error) => panic!("unsupported EFI command line: {error:?}"),
    }
}

fn regular_file(file: uefi::proto::media::file::FileHandle) -> RegularFile {
    match file.into_type().expect("failed to inspect UEFI boot file") {
        FileType::Regular(file) => file,
        FileType::Dir(_) => panic!("UEFI boot payload path is a directory"),
    }
}

fn file_size(file: &mut RegularFile) -> usize {
    #[repr(align(8))]
    struct InfoBuffer([u8; 512]);
    let mut buffer = InfoBuffer([0; 512]);
    let size = file
        .get_info::<FileInfo>(&mut buffer.0)
        .expect("failed to read UEFI boot file info")
        .file_size();
    usize::try_from(size).expect("UEFI boot file size overflows")
}

fn read_exact_file(file: &mut RegularFile, bytes: &mut [u8]) {
    let mut done = 0;
    while done < bytes.len() {
        let read = file
            .read(&mut bytes[done..])
            .expect("failed to read UEFI boot file");
        assert!(read != 0, "truncated UEFI boot file");
        done += read;
    }
}

pub(crate) fn boot_entropy() -> Option<[u8; 32]> {
    if !is_uefi_available() {
        return None;
    }

    let handle = boot::get_handle_for_protocol::<Rng>().ok()?;
    let mut rng = boot::open_protocol_exclusive::<Rng>(handle).ok()?;
    let mut seed = [0; 32];
    rng.get_rng(None, &mut seed).ok()?;
    Some(seed)
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
    find_fdt();
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

fn find_fdt() {
    with_config_table(|config_table| {
        if let Some(addr) = find_fdt_address(config_table) {
            let fdt_addr =
                <crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(addr as usize);
            if crate::fdt::set_fdt_addr_phys_if_valid(fdt_addr) {
                println!("Found FDT at address: {:p}", addr);
            } else {
                println!("Ignoring invalid FDT at address: {:p}", addr);
            }
        } else {
            println!("No FDT found in UEFI config tables.");
        }
    })
}

fn find_fdt_address(config_table: &[ConfigTableEntry]) -> Option<*const c_void> {
    config_table
        .iter()
        .find(|entry| entry.guid == FDT_TABLE_GUID)
        .map(|entry| entry.address)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fdt_table_address_by_uefi_guid() {
        let fdt_addr = 0x1234_0000usize as *const c_void;
        let acpi_addr = 0x5678_0000usize as *const c_void;
        let tables = [
            ConfigTableEntry {
                guid: ConfigTableEntry::ACPI2_GUID,
                address: acpi_addr,
            },
            ConfigTableEntry {
                guid: FDT_TABLE_GUID,
                address: fdt_addr,
            },
        ];

        assert_eq!(find_fdt_address(&tables), Some(fdt_addr));
    }

    #[test]
    fn boot_entropy_is_unavailable_without_uefi_system_table() {
        assert_eq!(boot_entropy(), None);
    }

    #[test]
    fn decodes_valid_ascii_efi_command_lines() {
        let mut bytes = [0_u8; MAX_CMDLINE];
        assert_eq!(
            decode_efi_cmdline(&[b'a' as u16; MAX_CMDLINE - 1], &mut bytes).unwrap(),
            "a".repeat(MAX_CMDLINE - 1)
        );
        assert_eq!(decode_efi_cmdline(&[], &mut bytes).unwrap(), "");
        assert_eq!(
            decode_efi_cmdline(&[b'a' as u16, b' ', b'=' as u16], &mut bytes).unwrap(),
            "a ="
        );
    }

    #[test]
    fn rejects_unsupported_efi_command_lines() {
        let mut bytes = [0_u8; MAX_CMDLINE];
        assert_eq!(
            decode_efi_cmdline(&vec![b'a' as u16; MAX_CMDLINE], &mut bytes),
            Err(EfiCmdlineError::TooLong)
        );
        for words in [&[0x100_u16][..], &[b'\t' as u16][..], &[b'\n' as u16][..]] {
            assert_eq!(
                decode_efi_cmdline(words, &mut bytes),
                Err(EfiCmdlineError::InvalidCharacter)
            );
        }
    }
}

pub fn is_uefi_available() -> bool {
    uefi::table::system_table_raw().is_some()
}

#[cfg(target_arch = "loongarch64")]
pub fn reset(reset_type: ResetType, status: Status, data: Option<&[u8]>) -> ! {
    info!("Resetting system via UEFI...");
    uefi::runtime::reset(reset_type, status, data)
}
