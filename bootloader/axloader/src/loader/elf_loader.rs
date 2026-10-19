use core::ptr::NonNull;

use axloader::{
    elf::{ElfError, ElfImage, LoadSegment},
    target::X86_64,
};
use someboot::boot_resources::{self, Allocation, Placement, ResourceId, ResourceRequest};

const UEFI_PAGE_SIZE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfLoadError {
    Elf(ElfError),
    AllocateFailed,
    SegmentAddressOverflow,
}

impl From<ElfError> for ElfLoadError {
    fn from(error: ElfError) -> Self {
        Self::Elf(error)
    }
}

#[derive(Debug)]
pub struct LoadedElf {
    pub entry_point: u64,
    pub load_addr: u64,
    pub load_end: u64,
    _allocation: Allocation,
}

pub(super) fn load_elf(
    image: &[u8],
    entry_symbol: Option<&str>,
) -> Result<LoadedElf, ElfLoadError> {
    let parsed = ElfImage::parse(image, X86_64, entry_symbol)?;
    let link_load_address = parsed.link_load_address();
    let entry = parsed.entry_point();
    let page_count = parsed.page_count();
    let allocation = allocate_load_region(link_load_address, page_count)?;
    let actual_load_addr = allocation.range().start;
    let target = NonNull::new(actual_load_addr as *mut u8).ok_or(ElfLoadError::AllocateFailed)?;

    copy_segments(image, parsed.segments(), link_load_address, target)?;

    let actual_load_end = actual_load_addr
        .checked_add(
            page_count
                .checked_mul(UEFI_PAGE_SIZE)
                .ok_or(ElfLoadError::SegmentAddressOverflow)? as u64,
        )
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
) -> Result<Allocation, ElfLoadError> {
    let bytes = (page_count as u64)
        .checked_mul(UEFI_PAGE_SIZE as u64)
        .ok_or(ElfLoadError::SegmentAddressOverflow)?;
    let request =
        |placement| ResourceRequest::new(ResourceId(1), bytes, UEFI_PAGE_SIZE as u64, placement);
    match boot_resources::reserve(request(Placement::At(preferred_load_addr))) {
        Ok(allocation) => Ok(allocation),
        Err(_) => {
            // Preserve the existing x86 load-bias contract for EFI PIE images.
            crate::logln!(
                "elf_load_relocate: preferred={:#x} pages={}",
                preferred_load_addr,
                page_count
            );
            boot_resources::reserve(request(Placement::Anywhere))
                .map_err(|_| ElfLoadError::AllocateFailed)
        }
    }
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
        let memory_size = usize::try_from(segment.memory_size)
            .map_err(|_| ElfLoadError::SegmentAddressOverflow)?;
        if memory_size > file_size {
            // SAFETY: `ElfImage::parse` proved `filesz <= memsz`, and both
            // offsets remain inside the reserved image span.
            unsafe {
                core::ptr::write_bytes(destination.add(file_size), 0, memory_size - file_size)
            };
        }
    }
    Ok(())
}
