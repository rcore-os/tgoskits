#[cfg(target_arch = "x86_64")]
use core::arch::naked_asm;
use core::{
    ffi::c_void,
    fmt::Write,
    mem::{MaybeUninit, align_of, size_of},
    ptr::{addr_of_mut, null},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub use uefi::Status;
#[cfg(target_arch = "loongarch64")]
pub use uefi::runtime::ResetType;
use uefi::{
    Guid, Result,
    boot::{self, AllocateType, MemoryDescriptor, MemoryType},
    guid,
    prelude::*,
    proto::{
        device_path::build,
        loaded_image::LoadedImage,
        media::{
            file::{File, FileAttribute, FileInfo, FileMode, FileType, RegularFile},
            load_file::LoadFile2,
        },
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
const LINUX_EFI_INITRD_MEDIA_GUID: Guid = guid!("5568e427-68fc-4f3d-ac74-ca555231cc68");
const LINUX_INITRD_DEVICE_PATH_SIZE: usize = 24;
const MAX_UEFI_INITRAMFS_BYTES: usize = 1024 * 1024 * 1024;
const MAX_CMDLINE: usize = 4095;

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
const LOADER_IMAGE_POOL_PAGES: usize =
    (crate::boot_payload::LOONG_IMAGE_POOL_BYTES as usize) / 4096;
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
const LOADER_ARCHIVE_POOL_PAGES: usize =
    (crate::boot_payload::LOONG_ARCHIVE_POOL_BYTES as usize) / 4096;
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
const LOADER_SLOT_POOL_PAGES: usize = (crate::boot_payload::LOONG_SLOT_POOL_BYTES as usize) / 4096;
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
const LOADER_IMAGE_POOL_BASE: u64 = 0x20_0000;

const _: () =
    assert!(size_of::<LoadFile2>() == size_of::<uefi_raw::protocol::media::LoadFile2Protocol>());
const _: () =
    assert!(align_of::<LoadFile2>() == align_of::<uefi_raw::protocol::media::LoadFile2Protocol>());

#[repr(align(8))]
struct AlignedBytes<const N: usize>([u8; N]);

static mut EXIT_BOOT_MEMORY_MAP_BUFFER: AlignedBytes<EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE> =
    AlignedBytes([0; EXIT_BOOT_MEMORY_MAP_BUFFER_SIZE]);
static mut EXIT_BOOT_MEMORY_MAP_DESCRIPTORS: [MaybeUninit<MemoryDescriptor>;
    EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY] =
    [const { MaybeUninit::uninit() }; EXIT_BOOT_MEMORY_MAP_DESCRIPTOR_CAPACITY];
static EFI_IMAGE_HANDLE: AtomicUsize = AtomicUsize::new(EFI_IMAGE_HANDLE_UNSET);
static INHERITED_EFI_HANDOFF: AtomicBool = AtomicBool::new(false);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_IMAGE_POOL: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_ARCHIVE_POOL: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_SLOT_POOL: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_IMAGE_CURSOR: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_SLOT_CURSOR: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_PAYLOAD_TABLE: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_IMAGE_START: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_IMAGE_END: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_BASE_IMAGE_START: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static LOADER_BASE_IMAGE_END: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
static EFI_SYSTEM_TABLE: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn setup_service(system_table: *const ::core::ffi::c_void) {
    let system_table = normalize_system_table(system_table);
    unsafe { table::set_system_table(system_table.cast()) };
    let inherited_handoff = inherited_handoff_available();
    #[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
    {
        EFI_SYSTEM_TABLE.store(system_table as usize, Ordering::Release);
        if inherited_handoff {
            restore_inherited_loader_context();
        } else {
            initialize_loader_pool();
        }
    }
    if let Some(image_handle) = saved_image_handle() {
        unsafe { boot::set_image_handle(image_handle) };
    }
    if !inherited_handoff {
        setup_console();
        println!("UEFI console ok.");
    }
    find_fdt();
    find_acpi_rsdp();
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
fn initialize_loader_pool() {
    if LOADER_IMAGE_POOL.load(Ordering::Acquire) != 0 {
        return;
    }
    let image_handle = boot::image_handle();
    let loaded_image = boot::open_protocol_exclusive::<LoadedImage>(image_handle)
        .expect("failed to inspect the LoongArch loader image");
    let (image_base, image_size) = loaded_image.info();
    let image_start = physical_alias(image_base as usize)
        .expect("LoongArch loader image address is not a physical alias");
    let image_size = usize::try_from(image_size).expect("LoongArch loader image size overflows");
    let image_end = image_start
        .checked_add(image_size)
        .expect("LoongArch loader image range overflows");
    drop(loaded_image);
    let image_pool_bytes = LOADER_IMAGE_POOL_PAGES
        .checked_mul(4096)
        .expect("LoongArch EFI image pool size overflows");
    let image_pool_start = usize::try_from(LOADER_IMAGE_POOL_BASE)
        .expect("LoongArch EFI image pool base overflows usize");
    let image_pool_end = image_pool_start
        .checked_add(image_pool_bytes)
        .expect("LoongArch EFI image pool range overflows");
    if image_start < image_pool_end && image_pool_start < image_end {
        panic!("LoongArch EFI image pool overlaps the running loader");
    }
    let image_pages = boot::allocate_pages(
        AllocateType::Address(LOADER_IMAGE_POOL_BASE),
        MemoryType::LOADER_DATA,
        LOADER_IMAGE_POOL_PAGES,
    )
    .expect("failed to reserve the LoongArch EFI image pool");
    let archive = boot::allocate_pages(
        AllocateType::AnyPages,
        MemoryType::LOADER_DATA,
        LOADER_ARCHIVE_POOL_PAGES,
    )
    .expect("failed to reserve the LoongArch EFI archive pool");
    let slot = boot::allocate_pages(
        AllocateType::AnyPages,
        MemoryType::LOADER_DATA,
        LOADER_SLOT_POOL_PAGES,
    )
    .expect("failed to reserve the LoongArch EFI slot pool");
    let table = boot::allocate_pool(
        MemoryType::RUNTIME_SERVICES_DATA,
        size_of::<crate::boot_payload::LoongBootPayload>(),
    )
    .expect("failed to reserve the LoongArch EFI payload table");
    let image_pages_start = physical_alias(image_pages.as_ptr() as usize)
        .expect("LoongArch EFI image pool address is not a physical alias");
    let archive_start = physical_alias(archive.as_ptr() as usize)
        .expect("LoongArch EFI archive pool address is not a physical alias");
    let slot_start = physical_alias(slot.as_ptr() as usize)
        .expect("LoongArch EFI slot pool address is not a physical alias");
    let table_start = physical_alias(table.as_ptr() as usize)
        .expect("LoongArch EFI payload table address is not a physical alias");
    let overlaps_image = |start: usize, pages: usize| {
        let end = start
            .checked_add(pages.checked_mul(4096).expect("EFI pool size overflows"))
            .expect("EFI pool range overflows");
        start < image_end && image_start < end
    };
    if overlaps_image(archive_start, LOADER_ARCHIVE_POOL_PAGES)
        || overlaps_image(slot_start, LOADER_SLOT_POOL_PAGES)
    {
        panic!("LoongArch EFI pool overlaps the running loader");
    }
    // SAFETY: all ranges are owned by this loader and remain reserved until reset.
    unsafe {
        let table_virt = if Arch::is_mmu_enabled() {
            Arch::_va(table_start)
        } else {
            table_start as *mut u8
        };
        table_virt
            .cast::<crate::boot_payload::LoongBootPayload>()
            .write(crate::boot_payload::LoongBootPayload::empty());
        boot::install_configuration_table(
            &crate::boot_payload::LOONG_BOOT_PAYLOAD_GUID,
            table_start as *mut c_void,
        )
        .expect("failed to install the LoongArch EFI payload table");
    }
    LOADER_IMAGE_POOL.store(image_pages_start, Ordering::Release);
    LOADER_ARCHIVE_POOL.store(archive_start, Ordering::Release);
    LOADER_SLOT_POOL.store(slot_start, Ordering::Release);
    LOADER_IMAGE_CURSOR.store(image_pages_start, Ordering::Release);
    LOADER_SLOT_CURSOR.store(slot_start, Ordering::Release);
    LOADER_PAYLOAD_TABLE.store(table_start, Ordering::Release);
    LOADER_IMAGE_START.store(image_start, Ordering::Release);
    LOADER_IMAGE_END.store(image_end, Ordering::Release);
    LOADER_BASE_IMAGE_START.store(image_start, Ordering::Release);
    LOADER_BASE_IMAGE_END.store(image_end, Ordering::Release);
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
fn restore_inherited_loader_context() {
    let payload = crate::boot_payload::inherited_payload()
        .expect("missing inherited LoongArch loader payload");
    payload
        .validate()
        .expect("invalid inherited LoongArch loader context");
    let physical_table = if Arch::is_mmu_enabled() {
        Arch::virt_to_phys(payload as *const _ as *const u8)
    } else {
        payload as *const _ as usize
    };
    let to_usize = |value: u64, name: &str| {
        usize::try_from(value).unwrap_or_else(|_| panic!("inherited {name} overflows usize"))
    };
    LOADER_IMAGE_POOL.store(
        to_usize(payload.image_pool_start, "image pool"),
        Ordering::Release,
    );
    LOADER_ARCHIVE_POOL.store(
        to_usize(payload.archive_pool_start, "archive pool"),
        Ordering::Release,
    );
    LOADER_SLOT_POOL.store(
        to_usize(payload.slot_pool_start, "slot pool"),
        Ordering::Release,
    );
    LOADER_IMAGE_CURSOR.store(
        to_usize(payload.image_pool_cursor, "image cursor"),
        Ordering::Release,
    );
    LOADER_SLOT_CURSOR.store(
        to_usize(payload.slot_pool_cursor, "slot cursor"),
        Ordering::Release,
    );
    LOADER_IMAGE_START.store(
        to_usize(payload.current_image_start, "current image"),
        Ordering::Release,
    );
    LOADER_IMAGE_END.store(
        to_usize(
            payload
                .current_image_start
                .checked_add(payload.current_image_len)
                .expect("inherited current image range overflows"),
            "current image end",
        ),
        Ordering::Release,
    );
    LOADER_BASE_IMAGE_START.store(
        to_usize(payload.current_image_start, "current image"),
        Ordering::Release,
    );
    LOADER_BASE_IMAGE_END.store(
        to_usize(
            payload
                .current_image_start
                .checked_add(payload.current_image_len)
                .expect("inherited current image range overflows"),
            "current image end",
        ),
        Ordering::Release,
    );
    LOADER_PAYLOAD_TABLE.store(physical_table, Ordering::Release);
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
pub fn loader_allocate_image(pages: usize) -> Option<usize> {
    allocate_from_pool(
        &LOADER_IMAGE_CURSOR,
        LOADER_IMAGE_POOL.load(Ordering::Acquire),
        LOADER_IMAGE_POOL_PAGES,
        pages,
    )
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
pub fn loader_allocate_slot(pages: usize) -> Option<usize> {
    allocate_from_pool(
        &LOADER_SLOT_CURSOR,
        LOADER_SLOT_POOL.load(Ordering::Acquire),
        LOADER_SLOT_POOL_PAGES,
        pages,
    )
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
pub fn loader_release_image(start: usize, pages: usize) -> bool {
    release_from_pool(
        &LOADER_IMAGE_CURSOR,
        LOADER_IMAGE_POOL.load(Ordering::Acquire),
        pages,
        start,
    )
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
pub fn loader_release_slot(start: usize, pages: usize) -> bool {
    release_from_pool(
        &LOADER_SLOT_CURSOR,
        LOADER_SLOT_POOL.load(Ordering::Acquire),
        pages,
        start,
    )
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
fn allocate_from_pool(
    cursor: &AtomicUsize,
    pool_start: usize,
    pool_pages: usize,
    pages: usize,
) -> Option<usize> {
    if pool_start == 0 || pages == 0 || pages > pool_pages {
        return None;
    }
    let bytes = pages.checked_mul(4096)?;
    let pool_end = pool_start.checked_add(pool_pages.checked_mul(4096)?)?;
    let mut current = cursor.load(Ordering::Acquire);
    loop {
        let end = current.checked_add(bytes)?;
        if current < pool_start || end > pool_end {
            return None;
        }
        match cursor.compare_exchange(current, end, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(current),
            Err(next) => current = next,
        }
    }
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
fn release_from_pool(cursor: &AtomicUsize, pool_start: usize, pages: usize, start: usize) -> bool {
    let bytes = match pages.checked_mul(4096) {
        Some(bytes) if start >= pool_start => bytes,
        _ => return false,
    };
    let end = match start.checked_add(bytes) {
        Some(end) => end,
        None => return false,
    };
    cursor
        .compare_exchange(end, start, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
pub fn loader_image_range() -> Option<(usize, usize)> {
    let start = LOADER_IMAGE_START.load(Ordering::Acquire);
    let end = LOADER_IMAGE_END.load(Ordering::Acquire);
    (start < end).then_some((start, end))
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
pub fn loader_set_slot(slot: Option<u8>) -> core::result::Result<(), &'static str> {
    let table = LOADER_PAYLOAD_TABLE.load(Ordering::Acquire);
    if table == 0 || !matches!(slot, None | Some(0 | 1)) {
        return Err("EFI loader payload table is unavailable");
    }
    let table = if Arch::is_mmu_enabled() {
        Arch::_va(table) as usize
    } else {
        table
    };
    // SAFETY: the table was allocated by initialize_loader_pool and remains live
    // until the inherited target returns.
    unsafe {
        (*((table) as *mut crate::boot_payload::LoongBootPayload)).loader_slot =
            slot.unwrap_or(u8::MAX);
    }
    Ok(())
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
pub fn loader_flush_icache() {
    ax_cpu::cache::flush_icache_all();
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
pub fn loader_set_payload_with_image(
    cmdline: Option<&str>,
    archive: Option<&[u8]>,
    target_image: Option<(usize, usize)>,
) -> core::result::Result<(), &'static str> {
    let table = LOADER_PAYLOAD_TABLE.load(Ordering::Acquire);
    let image_pool = LOADER_IMAGE_POOL.load(Ordering::Acquire);
    let archive_pool = LOADER_ARCHIVE_POOL.load(Ordering::Acquire);
    let slot_pool = LOADER_SLOT_POOL.load(Ordering::Acquire);
    if table == 0 || image_pool == 0 || archive_pool == 0 || slot_pool == 0 {
        return Err("EFI loader pool is unavailable");
    }
    if cmdline.is_none() && archive.is_none() && target_image.is_none() {
        let table = if Arch::is_mmu_enabled() {
            Arch::_va(table) as usize
        } else {
            table
        };
        // SAFETY: the table was allocated by initialize_loader_pool and remains live
        // until the loader reset or the inherited target returns.
        unsafe {
            *(table as *mut crate::boot_payload::LoongBootPayload) =
                crate::boot_payload::LoongBootPayload::empty();
        }
        LOADER_IMAGE_START.store(
            LOADER_BASE_IMAGE_START.load(Ordering::Acquire),
            Ordering::Release,
        );
        LOADER_IMAGE_END.store(
            LOADER_BASE_IMAGE_END.load(Ordering::Acquire),
            Ordering::Release,
        );
        return Ok(());
    }
    let (loader_start, loader_end) =
        loader_image_range().ok_or("current loader image range is unavailable")?;
    let memory_map = crate::mem::memory_map();
    if memory_map.len() > 128 {
        return Err("host memory map is too large");
    }

    let (current_image_start, current_image_end) = if let Some((start, end)) = target_image {
        if start >= end {
            return Err("invalid target image range");
        }
        if start < loader_end && loader_start < end {
            return Err("target image overlaps the loader");
        }
        let image_pool_end = image_pool
            .checked_add(LOADER_IMAGE_POOL_PAGES.checked_mul(4096).unwrap())
            .ok_or("EFI image pool range overflows")?;
        let slot_pool_end = slot_pool
            .checked_add(LOADER_SLOT_POOL_PAGES.checked_mul(4096).unwrap())
            .ok_or("EFI slot pool range overflows")?;
        let in_image_pool =
            start >= image_pool && end <= image_pool_end && start < end && start % 4096 == 0;
        let in_slot_pool =
            start >= slot_pool && end <= slot_pool_end && start < end && start % 4096 == 0;
        if !in_image_pool && !in_slot_pool {
            return Err("target image is outside the EFI pools");
        }
        (start, end)
    } else {
        (loader_start, loader_end)
    };
    if let Some(archive) = archive
        && archive.len() > LOADER_ARCHIVE_POOL_PAGES * 4096
    {
        return Err("host archive is too large");
    }

    let mut next = crate::boot_payload::LoongBootPayload::empty();
    next.image_pool_start = image_pool as u64;
    next.image_pool_len = (LOADER_IMAGE_POOL_PAGES * 4096) as u64;
    next.image_pool_cursor = LOADER_IMAGE_CURSOR.load(Ordering::Acquire) as u64;
    next.archive_pool_start = archive_pool as u64;
    next.archive_pool_len = (LOADER_ARCHIVE_POOL_PAGES * 4096) as u64;
    next.slot_pool_start = slot_pool as u64;
    next.slot_pool_len = (LOADER_SLOT_POOL_PAGES * 4096) as u64;
    next.slot_pool_cursor = LOADER_SLOT_CURSOR.load(Ordering::Acquire) as u64;
    next.current_image_start = current_image_start as u64;
    next.current_image_len = (current_image_end - current_image_start) as u64;
    if let Some(cmdline) = cmdline {
        next.set_cmdline(cmdline)?;
    }
    if let Some(archive) = archive {
        let destination = if Arch::is_mmu_enabled() {
            Arch::_va(archive_pool)
        } else {
            archive_pool as *mut u8
        };
        // SAFETY: the archive pool is reserved before ExitBootServices and the
        // checked length stays inside that pool.
        unsafe { core::ptr::copy_nonoverlapping(archive.as_ptr(), destination, archive.len()) };
        next.archive_start = archive_pool as u64;
        next.archive_len = archive.len() as u64;
    }
    if let Some((start, end)) = target_image {
        next.target_image_start = start as u64;
        next.target_image_len = (end - start) as u64;
    }
    for (index, region) in memory_map.iter().enumerate() {
        next.memory_map[index] = crate::boot_payload::LoongBootMemoryRegion {
            physical_start: region.physical_start as u64,
            size_in_bytes: region.size_in_bytes as u64,
            memory_type: match region.memory_type {
                crate::mem::MemoryType::Free => 0,
                crate::mem::MemoryType::Ram => 1,
                crate::mem::MemoryType::KImage => 2,
                crate::mem::MemoryType::Reserved => 3,
                crate::mem::MemoryType::Mmio => 4,
                crate::mem::MemoryType::PerCpuData => 5,
            },
            reserved: 0,
        };
    }
    next.memory_map_len = memory_map.len() as u32;
    next.validate()?;

    let table = if Arch::is_mmu_enabled() {
        Arch::_va(table) as usize
    } else {
        table
    };
    // SAFETY: the table was allocated by initialize_loader_pool and remains live
    // until the loader reset or the inherited target returns.
    let payload = unsafe { &mut *(table as *mut crate::boot_payload::LoongBootPayload) };
    *payload = next;
    if target_image.is_some() {
        LOADER_IMAGE_START.store(current_image_start, Ordering::Release);
        LOADER_IMAGE_END.store(current_image_end, Ordering::Release);
    }
    Ok(())
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
pub fn loader_efi_context() -> Option<(Handle, *const c_void)> {
    let image = saved_image_handle()?;
    let table = EFI_SYSTEM_TABLE.load(Ordering::Acquire);
    (table != 0).then(|| (image, normalize_system_table(table as *const c_void)))
}

#[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
fn reserve_loader_pool() {
    let reserve = |start: usize, pages: usize| {
        let descriptor = crate::mem::MemoryDescriptor::new_aligned(
            start,
            pages * 4096,
            crate::mem::MemoryType::Reserved,
            4096,
        );
        crate::mem::add_memory_descriptor(descriptor)
            .unwrap_or_else(|error| panic!("failed to reserve EFI loader pool: {error:?}"));
    };
    reserve(
        LOADER_IMAGE_POOL.load(Ordering::Acquire),
        LOADER_IMAGE_POOL_PAGES,
    );
    reserve(
        LOADER_ARCHIVE_POOL.load(Ordering::Acquire),
        LOADER_ARCHIVE_POOL_PAGES,
    );
    reserve(
        LOADER_SLOT_POOL.load(Ordering::Acquire),
        LOADER_SLOT_POOL_PAGES,
    );
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
        let inherited_handoff = inherited_handoff_available();
        INHERITED_EFI_HANDOFF.store(inherited_handoff, Ordering::Release);
        if !inherited_handoff {
            setup_console();
            println!("UEFI application started.");
            load_efi_cmdline(image_handle);
            load_efi_payload();
        } else {
            #[cfg(target_arch = "loongarch64")]
            load_host_boot_payload();
            #[cfg(not(target_arch = "loongarch64"))]
            unreachable!("inherited EFI handoff is only supported on LoongArch");
        }
        // Safety: `system_table` comes from the EFI firmware entry path and
        // matches the contract documented on `ArchTrait::efi_enter_kernel`.
        if Arch::efi_enter_kernel(system_table) {
            Status::SUCCESS
        } else {
            unreachable!()
        }
    }
}

/// Converts firmware table pointers to the LoongArch direct-map addresses used
/// after the first ArceOS EFI image has enabled its MMU.
fn normalize_system_table(system_table: *const c_void) -> *const c_void {
    #[cfg(target_arch = "loongarch64")]
    {
        if Arch::is_mmu_enabled()
            && let Some(physical) = physical_alias(system_table as usize)
        {
            let mapped = Arch::_va(physical).cast_const().cast::<c_void>();
            // SAFETY: `mapped` is the direct-map address of the firmware table
            // supplied by the EFI caller.
            unsafe { table::set_system_table(mapped.cast()) };
            let raw = table::system_table_raw()
                .expect("normalized EFI system table is unavailable")
                .as_ptr();
            // SAFETY: table storage pointers use the same firmware physical alias;
            // function pointers remain firmware addresses and are untouched.
            unsafe {
                (*raw).firmware_vendor = normalize_const_pointer((*raw).firmware_vendor);
                (*raw).stdin = normalize_mut_pointer((*raw).stdin);
                (*raw).stdout = normalize_mut_pointer((*raw).stdout);
                (*raw).stderr = normalize_mut_pointer((*raw).stderr);
                (*raw).runtime_services = normalize_mut_pointer((*raw).runtime_services);
                (*raw).boot_services = normalize_mut_pointer((*raw).boot_services);
                (*raw).configuration_table = normalize_mut_pointer((*raw).configuration_table);
            }
            return mapped;
        }
    }
    system_table
}

#[cfg(target_arch = "loongarch64")]
fn normalize_mut_pointer<T>(pointer: *mut T) -> *mut T {
    if Arch::is_mmu_enabled()
        && let Some(physical) = physical_alias(pointer as usize)
    {
        Arch::_va(physical).cast()
    } else {
        pointer
    }
}

#[cfg(target_arch = "loongarch64")]
fn normalize_const_pointer<T>(pointer: *const T) -> *const T {
    normalize_mut_pointer(pointer.cast_mut()).cast_const()
}

#[cfg(target_arch = "loongarch64")]
fn physical_alias(address: usize) -> Option<usize> {
    const PHYSICAL_ADDRESS_LIMIT: usize = 1 << 48;
    const PHYSICAL_MASK: usize = PHYSICAL_ADDRESS_LIMIT - 1;
    const ALIAS_BASES: [usize; 4] = [
        0x2000_0000_0000_0000,
        0x8000_0000_0000_0000,
        0x9000_0000_0000_0000,
        0xa000_0000_0000_0000,
    ];
    if address < PHYSICAL_ADDRESS_LIMIT {
        Some(address)
    } else if ALIAS_BASES.contains(&(address & !PHYSICAL_MASK)) {
        Some(address & PHYSICAL_MASK)
    } else {
        None
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
    if INHERITED_EFI_HANDOFF.load(Ordering::Acquire) {
        return;
    }
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
            #[cfg(target_arch = "loongarch64")]
            let virtual_system_table = st.as_ptr();
            #[cfg(not(target_arch = "loongarch64"))]
            let virtual_system_table = __va(st.as_ptr() as _) as _;
            set_virtual_address_map(&mut new_map, virtual_system_table)
                .expect("Failed to set virtual address map");
        }
    }

    memmap::setup_memory_map(mem_map.entries());
    #[cfg(all(feature = "efi-loader", target_arch = "loongarch64"))]
    reserve_loader_pool();
    crate::boot_payload::reserve_staged_uefi();
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn inherited_handoff() -> bool {
    INHERITED_EFI_HANDOFF.load(Ordering::Acquire)
}

#[cfg(not(target_arch = "loongarch64"))]
pub(crate) fn inherited_handoff() -> bool {
    false
}

#[cfg(target_arch = "loongarch64")]
fn inherited_handoff_available() -> bool {
    with_config_table(|tables| {
        tables.iter().any(|entry| {
            entry.guid == crate::boot_payload::LOONG_BOOT_PAYLOAD_GUID && !entry.address.is_null()
        })
    })
}

#[cfg(not(target_arch = "loongarch64"))]
fn inherited_handoff_available() -> bool {
    false
}

#[cfg(target_arch = "loongarch64")]
fn load_host_boot_payload() {
    let installed = with_config_table(|tables| {
        tables
            .iter()
            .find(|entry| entry.guid == crate::boot_payload::LOONG_BOOT_PAYLOAD_GUID)
            .map(|entry| entry.address)
    });
    let Some(address) = installed else {
        panic!("missing LoongArch inherited EFI payload");
    };
    assert!(!address.is_null(), "null LoongArch EFI payload table");
    let physical = physical_alias(address as usize).expect("invalid EFI payload address");
    let address = if Arch::is_mmu_enabled() {
        Arch::_va(physical).cast_const().cast::<c_void>()
    } else {
        address
    };
    // SAFETY: the first loader owns this runtime configuration-table allocation.
    let payload = unsafe { &*address.cast::<crate::boot_payload::LoongBootPayload>() };
    crate::boot_payload::remember_inherited_payload(payload);
    if payload.cmdline_len != 0 && !crate::cmdline::has_handoff_cmdline() {
        crate::cmdline::set_cmdline(payload.cmdline());
    }
    if payload.archive_len != 0 {
        let start = usize::try_from(payload.archive_start).expect("archive address overflows");
        let end = usize::try_from(
            payload
                .archive_start
                .checked_add(payload.archive_len)
                .expect("archive range overflows"),
        )
        .expect("archive end overflows");
        crate::boot_payload::publish(start, end, false);
    }
}

fn load_efi_payload() {
    let archive_staged = load_linux_initrd();
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
            size > 0 && size <= MAX_UEFI_INITRAMFS_BYTES,
            "invalid UEFI host archive size"
        );
        let pages = size.div_ceil(crate::consts::PAGE_SIZE);
        let image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
            .expect("failed to inspect the loaded UEFI image");
        let (image_base, _) = image.info();
        let max_address = (image_base as usize)
            .checked_sub(1)
            .expect("loaded UEFI image has no lower address for host archive");
        let address = boot::allocate_pages(
            AllocateType::MaxAddress(max_address as _),
            MemoryType::LOADER_DATA,
            pages,
        )
        .expect("failed to allocate UEFI host archive below the loaded image");
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

fn load_linux_initrd() -> bool {
    let mut storage = [MaybeUninit::uninit(); LINUX_INITRD_DEVICE_PATH_SIZE];
    let mut device_path = build::DevicePathBuilder::with_buf(&mut storage)
        .push(&build::media::Vendor {
            vendor_guid: LINUX_EFI_INITRD_MEDIA_GUID,
            vendor_defined_data: &[],
        })
        .expect("failed to build Linux initrd device path")
        .finalize()
        .expect("failed to finalize Linux initrd device path");
    let handle = match boot::locate_device_path::<LoadFile2>(&mut device_path) {
        Ok(handle) => handle,
        Err(error) if error.status() == Status::NOT_FOUND => return false,
        Err(error) => panic!("failed to locate Linux initrd provider: {error:?}"),
    };
    let mut provider = boot::open_protocol_exclusive::<LoadFile2>(handle)
        .expect("failed to open Linux initrd provider");
    // SAFETY: `open_protocol_exclusive` keeps this firmware protocol instance
    // valid and uniquely borrowed for the lifetime of `provider`. `LoadFile2`
    // is a `#[repr(transparent)]` wrapper around the raw UEFI protocol, and
    // the size and alignment assertions above pin that representation at
    // compile time.
    let raw_provider = unsafe {
        &mut *((&mut *provider) as *mut LoadFile2
            as *mut uefi_raw::protocol::media::LoadFile2Protocol)
    };
    let mut size = 0usize;
    // SAFETY: the firmware-installed callback belongs to `raw_provider`; the
    // finalized device path and `size` pointer are valid for this call, and a
    // null buffer with `BootPolicy = FALSE` is the UEFI LoadFile2 size-query
    // contract. The firmware does not retain any of these pointers.
    let status = unsafe {
        (raw_provider.load_file)(
            raw_provider,
            device_path.as_ffi_ptr().cast(),
            uefi_raw::Boolean::FALSE,
            &mut size,
            core::ptr::null_mut(),
        )
    };
    assert_eq!(
        status,
        uefi::Status::BUFFER_TOO_SMALL,
        "Linux initrd provider did not report its size"
    );
    assert!(
        size > 0 && size <= MAX_UEFI_INITRAMFS_BYTES,
        "invalid Linux initrd size: {size}"
    );
    let pages = size.div_ceil(crate::consts::PAGE_SIZE);
    let address = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
        .expect("failed to allocate Linux initrd");
    let mut loaded = size;
    // SAFETY: this is the same exclusively opened provider and finalized
    // device path as the size query. `address` owns `pages` writable
    // `LOADER_DATA` pages, and the exact size reported by the query is passed
    // as the buffer capacity, so the firmware callback may write only within
    // that allocation through `address` and `loaded`.
    let status = unsafe {
        (raw_provider.load_file)(
            raw_provider,
            device_path.as_ffi_ptr().cast(),
            uefi_raw::Boolean::FALSE,
            &mut loaded,
            address.as_ptr().cast(),
        )
    };
    if status != Status::SUCCESS || loaded != size {
        // SAFETY: the allocation has not been handed to the kernel.
        unsafe { boot::free_pages(address, pages) }
            .expect("failed to free invalid Linux initrd allocation");
        panic!("Linux initrd provider read failed: {status:?}, bytes={loaded}");
    }
    let start = address.as_ptr() as usize;
    let end = start
        .checked_add(size)
        .expect("Linux initrd range overflows");
    crate::boot_payload::stage_uefi(start, end);
    println!("Linux EFI initrd loaded: {size} bytes");
    true
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
    // An inherited LoongArch target is entered after the first loader has
    // exited boot services. Firmware protocol calls (including RNG discovery)
    // are no longer valid on that path; the target may still use a trusted FDT
    // seed or explicitly report that no firmware entropy is available.
    if INHERITED_EFI_HANDOFF.load(Ordering::Acquire) || !is_uefi_available() {
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

        // LoongArch firmware publishes callable service entry points in its
        // current EFI address space. Translate only the service-table storage
        // above; rewriting these function pointers to a guessed direct-map
        // alias breaks firmware-owned tables.
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
            let rsdp = <crate::arch::Arch as crate::ArchTrait>::canonicalize_paddr(addr as usize)
                as *const c_void;
            set_rsdp(rsdp);
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
    let system_table_address = uefi::table::system_table_raw()
        .map(|table| table.as_ptr() as usize)
        .unwrap_or_else(|| panic!("UEFI system table is unavailable during reset"));
    let system_table_address = if Arch::is_mmu_enabled()
        && let Some(physical) = physical_alias(system_table_address)
    {
        Arch::_va(physical) as usize
    } else {
        system_table_address
    };
    // The firmware may leave the runtime-services table at a physical or
    // firmware alias address after `SetVirtualAddressMap`; normalize that
    // storage pointer before reading its function entries.
    let runtime_services_address = unsafe {
        (*(system_table_address as *const uefi_raw::table::system::SystemTable)).runtime_services
            as usize
    };
    let runtime_services_address = if Arch::is_mmu_enabled()
        && let Some(physical) = physical_alias(runtime_services_address)
    {
        Arch::_va(physical) as usize
    } else {
        runtime_services_address
    };
    // SAFETY: the normalized pointer comes from the EFI system table and the
    // runtime-services table remains live after ExitBootServices.
    let runtime_services = unsafe {
        (runtime_services_address as *const uefi_raw::table::runtime::RuntimeServices)
            .as_ref()
            .unwrap_or_else(|| panic!("UEFI runtime services are unavailable during reset"))
    };
    let (data_size, data_ptr) =
        data.map_or((0, core::ptr::null()), |data| (data.len(), data.as_ptr()));
    // LoongArch keeps EFI function pointers in the firmware's active address
    // space. The table pointer was normalized above, but the entry point is
    // deliberately called unchanged; converting it without a firmware ABI
    // guarantee can jump into a different alias.
    // SAFETY: the runtime-services table owns this entry point and the data
    // slice stays alive for the duration of this non-returning call.
    unsafe { (runtime_services.reset_system)(reset_type, status, data_size, data_ptr) }
}
