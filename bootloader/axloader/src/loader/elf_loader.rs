use alloc::string::ToString;
use core::ptr::NonNull;

use axloader::{
    boot_job::operation_error,
    elf::{ElfError, ElfImage, LoadSegment},
    target::CURRENT,
};
use httpboot_protocol::DeviceOperationError;
use uefi::boot::{self, AllocateType, MemoryType};

const UEFI_PAGE_SIZE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ElfLoadError {
    #[error(transparent)]
    Elf(#[from] ElfError),
    #[error("ELF load allocation failed")]
    AllocateFailed,
    #[error("ELF load address or image span overflows")]
    SegmentAddressOverflow,
}

impl ElfLoadError {
    pub fn device_error(self) -> DeviceOperationError {
        match self {
            Self::Elf(error) => error.device_error(),
            error => operation_error("image_load_failed", error.to_string()),
        }
    }
}

#[derive(Debug)]
pub struct LoadedElf {
    pub entry_point: u64,
    pub load_addr: u64,
    pub load_end: u64,
    _allocation: ImagePages,
}

/// Owns executable firmware pages until return or ExitBootServices.
#[derive(Debug)]
struct ImagePages {
    pointer: NonNull<u8>,
    count: usize,
}

impl Drop for ImagePages {
    fn drop(&mut self) {
        let active = uefi::table::system_table_raw().is_some_and(|table| {
            // SAFETY: UEFI retains its system table after EBS and clears the
            // Boot Services pointer when those services become unavailable.
            !unsafe { table.as_ref() }.boot_services.is_null()
        });
        if active {
            // SAFETY: this guard exclusively owns the complete allocation;
            // no loaded-image reference survives its owner's destruction.
            unsafe { boot::free_pages(self.pointer, self.count) }
                .expect("failed to release ELF image pages");
        }
    }
}

pub(super) fn load_elf(image: &[u8]) -> Result<LoadedElf, ElfLoadError> {
    let parsed = ElfImage::parse(image, CURRENT)?;
    let link_load_address = parsed.link_load_address();
    let entry = parsed.entry_point();
    let page_count = parsed.page_count();
    let allocation = allocate_load_region(link_load_address, page_count)?;
    let target = allocation.pointer;
    let actual_load_addr = target.as_ptr() as u64;

    let allocation_bytes = page_count
        .checked_mul(UEFI_PAGE_SIZE)
        .ok_or(ElfLoadError::SegmentAddressOverflow)?;
    // UEFI page allocations are not a load protocol. Clear the complete span
    // so alignment gaps and other non-PT_LOAD bytes never expose stale memory.
    // SAFETY: the allocation guard owns `allocation_bytes` at `target`.
    unsafe { core::ptr::write_bytes(target.as_ptr(), 0, allocation_bytes) };
    copy_segments(image, parsed.segments(), link_load_address, target)?;
    super::arch::flush_instruction_cache();

    let actual_load_end = actual_load_addr
        .checked_add(allocation_bytes as u64)
        .ok_or(ElfLoadError::SegmentAddressOverflow)?;
    let entry = biased_addr(entry, link_load_address, actual_load_addr)
        .ok_or(ElfLoadError::SegmentAddressOverflow)?;

    Ok(LoadedElf {
        entry_point: entry,
        load_addr: actual_load_addr,
        load_end: actual_load_end,
        _allocation: allocation,
    })
}

fn biased_addr(addr: u64, original_base: u64, actual_base: u64) -> Option<u64> {
    let addr = addr as i128 + actual_base as i128 - original_base as i128;
    u64::try_from(addr).ok()
}

fn allocate_load_region(
    preferred_load_addr: u64,
    page_count: usize,
) -> Result<ImagePages, ElfLoadError> {
    // The image is entered before EBS, while firmware page permissions still
    // apply. Generic boot-resource reservations are LoaderData and may be NX.
    let allocate = |placement| boot::allocate_pages(placement, MemoryType::LOADER_CODE, page_count);
    let pointer = match allocate(AllocateType::Address(preferred_load_addr)) {
        Ok(pointer) => pointer,
        Err(_) => {
            // Relocatable EFI targets apply their own load bias at entry.
            crate::logln!(
                "elf_load_relocate: preferred={:#x} pages={}",
                preferred_load_addr,
                page_count
            );
            allocate(AllocateType::AnyPages).map_err(|_| ElfLoadError::AllocateFailed)?
        }
    };
    Ok(ImagePages {
        pointer,
        count: page_count,
    })
}

fn copy_segments(
    image: &[u8],
    segments: &[LoadSegment],
    load_addr: u64,
    target: NonNull<u8>,
) -> Result<(), ElfLoadError> {
    let base = target.as_ptr();
    for segment in segments {
        let destination_offset = usize::try_from(
            segment
                .physical_address
                .checked_sub(load_addr)
                .ok_or(ElfLoadError::SegmentAddressOverflow)?,
        )
        .map_err(|_| ElfLoadError::SegmentAddressOverflow)?;
        let file_start = usize::try_from(segment.file_offset)
            .map_err(|_| ElfLoadError::SegmentAddressOverflow)?;
        let file_size =
            usize::try_from(segment.file_size).map_err(|_| ElfLoadError::SegmentAddressOverflow)?;
        let file_end = file_start
            .checked_add(file_size)
            .ok_or(ElfLoadError::SegmentAddressOverflow)?;
        let source = image
            .get(file_start..file_end)
            .ok_or(ElfLoadError::SegmentAddressOverflow)?;
        // SAFETY: `ElfImage::parse` proved this destination lies between the
        // minimum PT_LOAD address and the page-aligned image end, and the
        // reservation returned a contiguous allocation covering that span.
        let destination = unsafe { base.add(destination_offset) };
        if !source.is_empty() {
            // SAFETY: `source` is a checked slice of the uploaded image, and
            // the destination is inside the unique load reservation.
            unsafe { core::ptr::copy_nonoverlapping(source.as_ptr(), destination, file_size) };
        }
    }
    Ok(())
}
