//! Optional EFI loader pools and payload producer.

use core::{
    ffi::c_void,
    mem::size_of,
    sync::atomic::{AtomicUsize, Ordering},
};

pub use payload::inherited_loader_slot;
use uefi::{
    Handle,
    boot::{self, AllocateType, MemoryType},
    proto::loaded_image::LoadedImage,
};

use super::{super::saved_image_handle, normalize_system_table, physical_alias};
use crate::{ArchTrait, arch::Arch, boot_payload::loongarch64 as payload};

const LOADER_IMAGE_POOL_PAGES: usize = (payload::LOONG_IMAGE_POOL_BYTES as usize) / 4096;
const LOADER_ARCHIVE_POOL_PAGES: usize = (payload::LOONG_ARCHIVE_POOL_BYTES as usize) / 4096;
const LOADER_SLOT_POOL_PAGES: usize = (payload::LOONG_SLOT_POOL_BYTES as usize) / 4096;
const LOADER_IMAGE_POOL_BASE: u64 = 0x20_0000;

static LOADER_IMAGE_POOL: AtomicUsize = AtomicUsize::new(0);
static LOADER_ARCHIVE_POOL: AtomicUsize = AtomicUsize::new(0);
static LOADER_SLOT_POOL: AtomicUsize = AtomicUsize::new(0);
static LOADER_IMAGE_CURSOR: AtomicUsize = AtomicUsize::new(0);
static LOADER_SLOT_CURSOR: AtomicUsize = AtomicUsize::new(0);
static LOADER_PAYLOAD_TABLE: AtomicUsize = AtomicUsize::new(0);
static LOADER_IMAGE_START: AtomicUsize = AtomicUsize::new(0);
static LOADER_IMAGE_END: AtomicUsize = AtomicUsize::new(0);
static LOADER_BASE_IMAGE_START: AtomicUsize = AtomicUsize::new(0);
static LOADER_BASE_IMAGE_END: AtomicUsize = AtomicUsize::new(0);
static EFI_SYSTEM_TABLE: AtomicUsize = AtomicUsize::new(0);

pub(super) fn prepare_service_context(system_table: *const c_void, inherited_handoff: bool) {
    EFI_SYSTEM_TABLE.store(system_table as usize, Ordering::Release);
    if inherited_handoff {
        restore_inherited_loader_context();
    } else {
        initialize_loader_pool();
    }
}

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
        size_of::<payload::LoongBootPayload>(),
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
            .cast::<payload::LoongBootPayload>()
            .write(payload::LoongBootPayload::empty());
        boot::install_configuration_table(
            &payload::LOONG_BOOT_PAYLOAD_GUID,
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

fn restore_inherited_loader_context() {
    let payload = payload::inherited_payload().expect("missing inherited LoongArch loader payload");
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

pub fn loader_allocate_image(pages: usize) -> Option<usize> {
    allocate_from_pool(
        &LOADER_IMAGE_CURSOR,
        LOADER_IMAGE_POOL.load(Ordering::Acquire),
        LOADER_IMAGE_POOL_PAGES,
        pages,
    )
}

pub fn loader_allocate_slot(pages: usize) -> Option<usize> {
    allocate_from_pool(
        &LOADER_SLOT_CURSOR,
        LOADER_SLOT_POOL.load(Ordering::Acquire),
        LOADER_SLOT_POOL_PAGES,
        pages,
    )
}

pub fn loader_release_image(start: usize, pages: usize) -> bool {
    release_from_pool(
        &LOADER_IMAGE_CURSOR,
        LOADER_IMAGE_POOL.load(Ordering::Acquire),
        pages,
        start,
    )
}

pub fn loader_release_slot(start: usize, pages: usize) -> bool {
    release_from_pool(
        &LOADER_SLOT_CURSOR,
        LOADER_SLOT_POOL.load(Ordering::Acquire),
        pages,
        start,
    )
}

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

pub fn loader_image_range() -> Option<(usize, usize)> {
    let start = LOADER_IMAGE_START.load(Ordering::Acquire);
    let end = LOADER_IMAGE_END.load(Ordering::Acquire);
    (start < end).then_some((start, end))
}

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
        (*((table) as *mut payload::LoongBootPayload)).loader_slot = slot.unwrap_or(u8::MAX);
    }
    Ok(())
}

pub fn loader_flush_icache() {
    ax_cpu::cache::flush_icache_all();
}

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
            *(table as *mut payload::LoongBootPayload) = payload::LoongBootPayload::empty();
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
        let in_image_pool = start >= image_pool
            && end <= image_pool_end
            && start < end
            && start.is_multiple_of(4096);
        let in_slot_pool =
            start >= slot_pool && end <= slot_pool_end && start < end && start.is_multiple_of(4096);
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

    let mut next = payload::LoongBootPayload::empty();
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
        next.memory_map[index] = payload::LoongBootMemoryRegion {
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
    let payload = unsafe { &mut *(table as *mut payload::LoongBootPayload) };
    *payload = next;
    if target_image.is_some() {
        LOADER_IMAGE_START.store(current_image_start, Ordering::Release);
        LOADER_IMAGE_END.store(current_image_end, Ordering::Release);
    }
    Ok(())
}

pub fn loader_efi_context() -> Option<(Handle, *const c_void)> {
    let image = saved_image_handle()?;
    let table = EFI_SYSTEM_TABLE.load(Ordering::Acquire);
    (table != 0).then(|| (image, normalize_system_table(table as *const c_void)))
}

pub(super) fn reserve_loader_pool() {
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
