//! Boot Services adapter. No call in this module is legal after EBS.
use core::ptr::NonNull;

use uefi::{
    boot::{self, AllocateType, MemoryType},
    proto::{
        device_path::media::FilePath,
        loaded_image::LoadedImage,
        media::file::{File, FileAttribute, FileInfo, FileMode, FileType},
    },
};

use super::{PAGE_SIZE, PageAllocator, PhysicalRange, Placement, ResourceError, ResourceRequest};

pub(crate) struct FirmwarePages;

fn physical(pointer: *mut u8) -> Result<u64, ResourceError> {
    cfg_select! {
        all(target_arch = "loongarch64", not(target_os = "uefi")) => {
            crate::efi_stub::loongarch64::physical_alias(pointer as usize)
                .map(|value| value as u64)
                .ok_or(ResourceError::InvalidSnapshot)
        }
        _ => Ok(pointer as u64),
    }
}

// SAFETY: EFI returns exclusive LOADER_DATA pages. Extra alignment pages are
// released before ownership is transferred, and release is used only pre-EBS.
unsafe impl PageAllocator for FirmwarePages {
    fn reserve(&mut self, request: ResourceRequest) -> Result<PhysicalRange, ResourceError> {
        request.validate()?;
        if !boot_services_active() {
            return Err(ResourceError::Unavailable);
        }
        let pages = request.capacity / PAGE_SIZE;
        let extra = match request.placement() {
            Placement::Anywhere => request.alignment / PAGE_SIZE - 1,
            Placement::At(_) => 0,
        };
        let total = pages
            .checked_add(extra)
            .and_then(|v| usize::try_from(v).ok())
            .filter(|v| v.checked_mul(PAGE_SIZE as usize).is_some())
            .ok_or(ResourceError::Capacity)?;
        let kind = match request.placement() {
            Placement::Anywhere => AllocateType::AnyPages,
            Placement::At(addr) => AllocateType::Address(addr),
        };
        let allocation = boot::allocate_pages(kind, MemoryType::LOADER_DATA, total)
            .map_err(|error| ResourceError::Firmware(error.status().0))?;
        let raw = match physical(allocation.as_ptr()) {
            Ok(raw) => raw,
            Err(error) => {
                // SAFETY: the just-allocated range has not been published.
                unsafe {
                    boot::free_pages(allocation, total)
                        .expect("failed to release invalid EFI reservation");
                }
                return Err(error);
            }
        };
        let aligned = raw
            .checked_add(request.alignment - 1)
            .map(|value| value & !(request.alignment - 1));
        let limit = raw.checked_add(total as u64 * PAGE_SIZE);
        let Some(aligned) = aligned.filter(|aligned| {
            aligned
                .checked_add(request.capacity)
                .zip(limit)
                .is_some_and(|(end, limit)| end <= limit)
        }) else {
            // SAFETY: the exclusive allocation has not been split or published.
            unsafe {
                boot::free_pages(allocation, total)
                    .expect("failed to release invalid EFI reservation");
            }
            return Err(ResourceError::Capacity);
        };
        let prefix = (aligned - raw) / PAGE_SIZE;
        let suffix = extra - prefix;
        // SAFETY: prefix and suffix are disjoint unused parts of this allocation.
        unsafe {
            if prefix != 0 {
                release(PhysicalRange::new(raw, prefix * PAGE_SIZE));
            }
            if suffix != 0 {
                release(PhysicalRange::new(
                    aligned + request.capacity,
                    suffix * PAGE_SIZE,
                ));
            }
        }
        Ok(PhysicalRange::new(aligned, request.capacity))
    }
    unsafe fn release(&mut self, range: PhysicalRange) {
        // SAFETY: forwarded from the allocator ownership contract.
        unsafe {
            release(range);
        }
    }
}

pub(crate) unsafe fn release(range: PhysicalRange) {
    // A surviving native guard becomes retained boot memory after EBS. Never
    // call a firmware service or deallocate pages now owned by the kernel.
    if !boot_services_active() {
        return;
    }
    let pointer = NonNull::new(range.start as *mut u8).expect("nonzero EFI allocation");
    // SAFETY: the caller relinquishes this complete exclusive page reservation.
    unsafe {
        boot::free_pages(pointer, (range.len / PAGE_SIZE) as usize)
            .expect("failed to release EFI resource");
    }
}

pub(crate) fn boot_services_active() -> bool {
    uefi::table::system_table_raw().is_some_and(|table| {
        // SAFETY: the EFI crate retains a valid system table, including after
        // EBS, when its BootServices pointer is cleared.
        !unsafe { table.as_ref() }.boot_services.is_null()
    })
}

/// Reads the original EFI file, never a relocated and mutated runtime image.
/// # Safety
/// The caller owns `range` exclusively for writes; it remains reserved after
/// this function, and no references to its contents have been published yet.
pub(crate) unsafe fn initialize_image(range: PhysicalRange) -> Result<u64, ResourceError> {
    if !boot_services_active() {
        return Err(ResourceError::Unavailable);
    }
    let handle = boot::image_handle();
    let image = boot::open_protocol_exclusive::<LoadedImage>(handle)
        .map_err(|error| ResourceError::Firmware(error.status().0))?;
    let path = image.file_path().ok_or(ResourceError::Unavailable)?;
    let mut words = [0u16; 4096];
    let mut len = 0;
    for node in path.node_iter() {
        let Ok(node) = <&FilePath>::try_from(node) else {
            continue;
        };
        let name = node.path_name();
        if name.is_empty() || name.len() > words.len() - len {
            return Err(ResourceError::Capacity);
        }
        name.copy_to(&mut words[len..len + name.len()]);
        // FilePath nodes each carry a terminator; concatenate their path parts.
        len += name.len();
        if words[len - 1] != 0 {
            return Err(ResourceError::InvalidRequest);
        }
        len -= 1;
    }
    if len == 0 {
        return Err(ResourceError::Unavailable);
    }
    let path = uefi::CStr16::from_u16_with_nul(&words[..=len])
        .map_err(|_| ResourceError::InvalidRequest)?;
    drop(image);
    let mut filesystem = boot::get_image_file_system(handle)
        .map_err(|error| ResourceError::Firmware(error.status().0))?;
    let mut volume = filesystem
        .open_volume()
        .map_err(|error| ResourceError::Firmware(error.status().0))?;
    let file = volume
        .open(path, FileMode::Read, FileAttribute::empty())
        .map_err(|error| ResourceError::Firmware(error.status().0))?;
    let FileType::Regular(mut file) = file
        .into_type()
        .map_err(|error| ResourceError::Firmware(error.status().0))?
    else {
        return Err(ResourceError::InvalidRequest);
    };
    #[repr(align(8))]
    struct FileInfoBuffer([u8; 1024]);
    let mut info = FileInfoBuffer([0; 1024]);
    let len = file
        .get_info::<FileInfo>(&mut info.0)
        .map_err(|error| ResourceError::Firmware(error.status().0))?
        .file_size();
    if len == 0 || len > range.len {
        return Err(ResourceError::Capacity);
    }
    cfg_select! {
        target_os = "uefi" => {
            let destination = range.start as *mut u8;
        }
        _ => {
            let destination = crate::mem::phys_to_virt(range.start as usize);
        }
    }
    // Firmware DMA must receive its own allocation address, never a kernel
    // direct-map alias. Copy bounded chunks into the reserved physical range.
    let scratch = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, 1)
        .map_err(|error| ResourceError::Firmware(error.status().0))?;
    struct Scratch(NonNull<u8>);
    impl Drop for Scratch {
        fn drop(&mut self) {
            // SAFETY: this unpublished single-page buffer is exclusive and
            // cannot escape this pre-EBS file operation.
            let _ = unsafe { boot::free_pages(self.0, 1) };
        }
    }
    let scratch = Scratch(scratch);
    // SAFETY: EFI granted one exclusive page. Initialize every byte before
    // creating a Rust slice that the safe firmware API can borrow.
    unsafe {
        core::ptr::write_bytes(scratch.0.as_ptr(), 0, PAGE_SIZE as usize);
    }
    // SAFETY: the initialized firmware page remains exclusively borrowed here.
    let bytes = unsafe { core::slice::from_raw_parts_mut(scratch.0.as_ptr(), PAGE_SIZE as usize) };
    let mut done = 0;
    while done < len as usize {
        let count = bytes.len().min(len as usize - done);
        let read = file
            .read(&mut bytes[..count])
            .map_err(|error| ResourceError::Firmware(error.status().0))?;
        if read == 0 {
            return Err(ResourceError::Unavailable);
        }
        // SAFETY: the source page is separate from the reserved destination,
        // and the file size was bounded by the destination capacity above.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), destination.add(done), read);
        }
        done += read;
    }
    Ok(len)
}
