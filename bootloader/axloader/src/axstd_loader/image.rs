//! Reserved image memory, payload ownership and terminal EFI entry.
use core::ptr::NonNull;

use axloader::{efi_image, elf::ElfImage};
use someboot::{
    boot_resources::{self, Allocation, PhysicalRange, ResourceId},
    handoff::{ApplicationMetadata, HandoffSpec, PreparedHandoff},
};

use super::{ARCHIVE_RESOURCE, IMAGE_RESOURCE, SLOT_RESOURCE};

#[derive(Debug)]
pub(super) struct LoadedElf {
    pub(super) entry_point: u64,
    pub(super) load_addr: u64,
    pub(super) load_end: u64,
    _allocation: Allocation,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ElfError {
    #[error(transparent)]
    Elf(axloader::elf::ElfError),
    #[error("ELF image conflicts with reserved memory")]
    AddressConflict,
    #[error("ELF load memory is unavailable")]
    Allocation,
    #[error("ELF load address overflows")]
    SegmentAddressOverflow,
    #[error("ELF physical load segment is not page-aligned")]
    SegmentNotPageAligned,
    #[error("ELF link address does not match the fixed image plan")]
    LinkAddressMismatch,
}

impl LoadedElf {
    pub(super) fn load(
        image: &[u8],
        spec: axloader::target::TargetSpec,
        entry_symbol: Option<&str>,
    ) -> Result<Self, ElfError> {
        let parsed = ElfImage::parse(image, spec, entry_symbol).map_err(ElfError::Elf)?;
        let link_addr = parsed.link_load_address();
        let entry = parsed.entry_point();
        let page_count = parsed.page_count();
        if parsed
            .segments()
            .iter()
            .any(|segment| segment.physical_address % 4096 != 0)
        {
            return Err(ElfError::SegmentNotPageAligned);
        }
        if spec
            .link_address
            .is_some_and(|expected| expected != link_addr)
        {
            return Err(ElfError::LinkAddressMismatch);
        }
        let allocation =
            reserve_image(IMAGE_RESOURCE, page_count).map_err(|_| ElfError::AddressConflict)?;
        let target_phys = allocation.range().start as usize;
        let image_span = page_count
            .checked_mul(4096)
            .ok_or(ElfError::SegmentAddressOverflow)?;
        let target_end = target_phys
            .checked_add(image_span)
            .ok_or(ElfError::AddressConflict)?;
        let (loader_start, loader_end) = match someboot::handoff::current_image() {
            Some(range) => (range.start as usize, range.end().unwrap() as usize),
            None => {
                return Err(ElfError::AddressConflict);
            }
        };
        if target_phys < loader_end && loader_start < target_end {
            return Err(ElfError::AddressConflict);
        }
        if spec
            .load_address
            .is_some_and(|expected| expected != target_phys as u64)
        {
            return Err(ElfError::AddressConflict);
        }
        let target = match NonNull::new(someboot::mem::phys_to_virt(target_phys)) {
            Some(target) => target,
            None => {
                return Err(ElfError::Allocation);
            }
        };
        let allocation_bytes = usize::try_from(allocation.range().len)
            .map_err(|_| ElfError::SegmentAddressOverflow)?;
        // The fixed image pool may be reused after a failed handoff. Clear the
        // complete allocation so alignment gaps between PT_LOAD segments never
        // expose bytes left by an earlier image.
        // SAFETY: `target` is the non-null virtual alias returned for the
        // contiguous `allocation_bytes` image allocation, and the validated
        // page count keeps the write within that allocation.
        unsafe { core::ptr::write_bytes(target.as_ptr(), 0, allocation_bytes) };
        copy_segments(image, parsed.segments(), link_addr, target)
            .map_err(|_| ElfError::SegmentAddressOverflow)?;
        someboot::handoff::flush_instruction_cache();
        Ok(Self {
            entry_point: someboot::mem::phys_to_virt(
                target_phys
                    + usize::try_from(entry - link_addr)
                        .map_err(|_| ElfError::SegmentAddressOverflow)?,
            ) as u64,
            load_addr: target_phys as u64,
            load_end: target_end as u64,
            _allocation: allocation,
        })
    }
}

#[derive(Debug)]
pub(super) struct LoadedPe {
    pub(super) entry_point: u64,
    pub(super) load_addr: usize,
    pub(super) load_end: usize,
    _allocation: Allocation,
}

impl LoadedPe {
    pub(super) fn validate(image: &[u8]) -> Result<(), &'static str> {
        let layout = efi_image::layout(image)?;
        layout.validate(image.len())
    }

    pub(super) fn load(image: &[u8]) -> Result<Self, &'static str> {
        let layout = efi_image::layout(image)?;
        layout.validate(image.len())?;
        let page_count = layout.image_size.div_ceil(4096);
        let allocation = reserve_image(SLOT_RESOURCE, page_count)?;
        let load_addr = allocation.range().start as usize;
        let target = someboot::mem::phys_to_virt(load_addr);
        // SAFETY: the slot pool is reserved by someboot and `image_size` stays
        // inside the fixed allocation returned above.
        unsafe {
            core::ptr::write_bytes(target, 0, page_count * 4096);
            core::ptr::copy_nonoverlapping(image.as_ptr(), target, layout.headers);
        }
        for section in layout.sections {
            // SAFETY: `efi_image::Layout::validate` checked each section's virtual range against
            // `image_size`, and `target` points to the allocation of
            // `page_count * 4096` bytes covering that complete image.
            let destination = unsafe { target.add(section.virtual_address) };
            if section.raw_size != 0 {
                // SAFETY: `efi_image::Layout::validate` checked the raw range and the destination
                // section range against the allocated image.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        image.as_ptr().add(section.raw_offset),
                        destination,
                        section.raw_size,
                    );
                }
            }
        }
        someboot::handoff::flush_instruction_cache();
        Ok(Self {
            entry_point: someboot::mem::phys_to_virt(load_addr + layout.entry) as u64,
            load_addr,
            load_end: load_addr + page_count * 4096,
            _allocation: allocation,
        })
    }
}

fn copy_segments(
    image: &[u8],
    segments: &[axloader::elf::LoadSegment],
    load_addr: u64,
    target: NonNull<u8>,
) -> Result<(), ElfError> {
    for segment in segments {
        let offset = usize::try_from(
            segment
                .physical_address
                .checked_sub(load_addr)
                .ok_or(ElfError::SegmentAddressOverflow)?,
        )
        .map_err(|_| ElfError::SegmentAddressOverflow)?;
        // SAFETY: `ElfImage::parse` proved this offset lies in the validated
        // PT_LOAD span, and the fixed image allocation covers that span.
        let destination = unsafe { target.as_ptr().add(offset) };
        let source =
            usize::try_from(segment.file_offset).map_err(|_| ElfError::SegmentAddressOverflow)?;
        let filesz =
            usize::try_from(segment.file_size).map_err(|_| ElfError::SegmentAddressOverflow)?;
        let memsz =
            usize::try_from(segment.memory_size).map_err(|_| ElfError::SegmentAddressOverflow)?;
        if filesz != 0 {
            // SAFETY: the source range was validated against the uploaded
            // image, while the destination remains inside the unique image
            // allocation.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    image
                        .get(
                            source
                                ..source
                                    .checked_add(filesz)
                                    .ok_or(ElfError::SegmentAddressOverflow)?,
                        )
                        .ok_or(ElfError::SegmentAddressOverflow)?
                        .as_ptr(),
                    destination,
                    filesz,
                );
            }
        }
        if memsz > filesz {
            // SAFETY: `filesz <= memsz` and the validated PT_LOAD span keeps
            // both the copied prefix and zeroed tail within the allocation.
            unsafe { core::ptr::write_bytes(destination.add(filesz), 0, memsz - filesz) };
        }
    }
    Ok(())
}

fn reserve_image(id: ResourceId, pages: usize) -> Result<Allocation, &'static str> {
    let bytes = pages
        .checked_mul(4096)
        .and_then(|bytes| bytes.checked_add(0x1f_ffff))
        .map(|bytes| bytes & !0x1f_ffff)
        .ok_or("image reservation overflows")?;
    boot_resources::allocate(id, bytes as u64, 0x20_0000).map_err(|_| "image resource unavailable")
}

/// The application has one control task and no outstanding storage operation.
/// Keep image and payload owners on this stack until the target takes over.
pub(super) fn enter_prepared<I>(payload: PublishedPayload, entry: u64, image: I) -> ! {
    use ax_std::os::arceos::api::modules::ax_runtime::{emergency_console, handoff};

    // SAFETY: both call sites have stopped application I/O and committed OTA
    // state. The runtime stops its own services before this closure; the target
    // EFI entry and reserved allocations were validated during preparation.
    unsafe {
        handoff::enter(move |context| {
            let _image = image;
            let sealed = match payload.0.seal(&context) {
                Ok(sealed) => sealed,
                Err(error) => {
                    emergency_console::write_fmt(format_args!(
                        "efi_handoff_seal_failed: {error}\n"
                    ));
                    return;
                }
            };
            // SAFETY: the runtime has quiesced tasks, IRQs and DMA; the owned
            // payload and caller's loaded image remain live throughout entry.
            match sealed.enter(entry) {
                Ok(()) => {
                    emergency_console::write_fmt(format_args!(
                        "efi_handoff_entry_returned: SUCCESS\n"
                    ));
                }
                Err(error) => {
                    emergency_console::write_fmt(format_args!(
                        "efi_handoff_entry_failed: {error}\n"
                    ));
                }
            }
        })
    }
}

pub(super) struct PublishedPayload(PreparedHandoff);
impl PublishedPayload {
    pub(super) fn new(
        initramfs: Option<&[u8]>,
        cmdline: Option<&str>,
        target_image: Option<(usize, usize)>,
    ) -> Result<Self, &'static str> {
        Self::with_metadata(initramfs, cmdline, target_image, None)
    }
    pub(super) fn with_metadata(
        initramfs: Option<&[u8]>,
        cmdline: Option<&str>,
        target_image: Option<(usize, usize)>,
        metadata: Option<ApplicationMetadata<'_>>,
    ) -> Result<Self, &'static str> {
        let (start, end) = target_image.ok_or("missing target image")?;
        let archive = initramfs
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| {
                boot_resources::allocate(
                    ARCHIVE_RESOURCE,
                    (bytes.len() as u64)
                        .checked_next_multiple_of(4096)
                        .ok_or("archive size overflows")?,
                    4096,
                )
                .map_err(|_| "archive resource unavailable")
            })
            .transpose()?;
        PreparedHandoff::prepare(
            HandoffSpec {
                preserve: &[],
                image: PhysicalRange::new(start as u64, (end - start) as u64),
                cmdline,
                initramfs,
                metadata,
            },
            archive,
        )
        .map(Self)
        .map_err(|_| "EFI handoff preparation failed")
    }
}

pub(super) struct BootExecution {
    pub(super) elf: LoadedElf,
    pub(super) payload: PublishedPayload,
}
impl BootExecution {
    pub(super) fn handoff(self) -> ! {
        enter_prepared(self.payload, self.elf.entry_point, self.elf)
    }
}
