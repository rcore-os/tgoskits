use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use ax_fs::{FileBackend, FileFlags};
use ax_hal::paging::{MappingFlags, PageSize};
use ax_memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange, align_up_4k};
use ax_task::current;
use linux_raw_sys::general::*;
use starry_vm::{vm_load, vm_write_slice};

use crate::{
    file::get_file_like,
    mm::{Backend, SharedPages},
    pseudofs::{Device, DeviceMmap},
    task::AsThread,
};

bitflags::bitflags! {
    /// `PROT_*` flags for use with [`sys_mmap`].
    ///
    /// For `PROT_NONE`, use `ProtFlags::empty()`.
    #[derive(Debug, Clone, Copy)]
    struct MmapProt: u32 {
        /// Page can be read.
        const READ = PROT_READ;
        /// Page can be written.
        const WRITE = PROT_WRITE;
        /// Page can be executed.
        const EXEC = PROT_EXEC;
        /// Extend change to start of growsdown vma (mprotect only).
        const GROWDOWN = PROT_GROWSDOWN;
        /// Extend change to start of growsup vma (mprotect only).
        const GROWSUP = PROT_GROWSUP;
    }
}

impl From<MmapProt> for MappingFlags {
    fn from(value: MmapProt) -> Self {
        let mut flags = MappingFlags::USER;
        if value.contains(MmapProt::READ) {
            flags |= MappingFlags::READ;
        }
        if value.contains(MmapProt::WRITE) {
            flags |= MappingFlags::WRITE;
        }
        if value.contains(MmapProt::EXEC) {
            flags |= MappingFlags::EXECUTE;
        }
        flags
    }
}

fn capped_device_map_len(request_len: usize, available_len: usize, page_size: PageSize) -> usize {
    request_len.min(available_len.align_up(page_size))
}

bitflags::bitflags! {
    /// flags for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    struct MmapFlags: u32 {
        /// Share changes
        const SHARED = MAP_SHARED;
        /// Share changes, but fail if mapping flags contain unknown
        const SHARED_VALIDATE = MAP_SHARED_VALIDATE;
        /// Changes private; copy pages on write.
        const PRIVATE = MAP_PRIVATE;
        /// Map address must be exactly as requested, no matter whether it is available.
        const FIXED = MAP_FIXED;
        /// Same as `FIXED`, but if the requested address overlaps an existing
        /// mapping, the call fails instead of replacing the existing mapping.
        const FIXED_NOREPLACE = MAP_FIXED_NOREPLACE;
        /// Don't use a file.
        const ANONYMOUS = MAP_ANONYMOUS;
        /// Populate the mapping.
        const POPULATE = MAP_POPULATE;
        /// Don't check for reservations.
        const NORESERVE = MAP_NORESERVE;
        /// Allocation is for a stack.
        const STACK = MAP_STACK;
        /// Huge page
        const HUGE = MAP_HUGETLB;
        /// Huge page 1g size
        const HUGE_1GB = MAP_HUGETLB | MAP_HUGE_1GB;
        /// Deprecated flag
        const DENYWRITE = MAP_DENYWRITE;

        /// Mask for type of mapping
        const TYPE = MAP_TYPE;
    }
}

pub fn sys_mmap(
    addr: usize,
    length: usize,
    prot: u32,
    flags: u32,
    fd: i32,
    offset: isize,
) -> AxResult<isize> {
    if length == 0 {
        return Err(AxError::InvalidInput);
    }

    let curr = current();
    let mut aspace = curr.as_thread().proc_data.aspace.lock();
    let permission_flags = MmapProt::from_bits_truncate(prot);
    // TODO: check illegal flags for mmap
    let map_flags = match MmapFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("unknown mmap flags: {flags}");
            if (flags & MmapFlags::TYPE.bits()) == MmapFlags::SHARED_VALIDATE.bits() {
                return Err(AxError::OperationNotSupported);
            }
            MmapFlags::from_bits_truncate(flags)
        }
    };
    // Exactly one of MAP_PRIVATE or MAP_SHARED must be set. `MAP_PRIVATE|MAP_SHARED`
    // shares the bit pattern 0x03 with `MAP_SHARED_VALIDATE`; Linux rejects this
    // ambiguous combo with EINVAL, and StarryOS does not implement `SHARED_VALIDATE`
    // semantics separately, so we reject 0x03 here too.
    let map_type = map_flags & MmapFlags::TYPE;
    let type_bits = map_type.bits();
    if type_bits != MAP_PRIVATE && type_bits != MAP_SHARED {
        return Err(AxError::InvalidInput);
    }
    if map_flags.contains(MmapFlags::ANONYMOUS) != (fd <= 0) {
        return Err(AxError::InvalidInput);
    }
    if fd <= 0 && offset != 0 {
        return Err(AxError::InvalidInput);
    }
    let offset: usize = offset.try_into().map_err(|_| AxError::InvalidInput)?;
    if !PageSize::Size4K.is_aligned(offset) {
        return Err(AxError::InvalidInput);
    }

    debug!(
        "sys_mmap <= addr: {addr:#x?}, length: {length:#x?}, prot: {permission_flags:?}, flags: \
         {map_flags:?}, fd: {fd:?}, offset: {offset:?}"
    );

    let page_size = if map_flags.contains(MmapFlags::HUGE_1GB) {
        PageSize::Size1G
    } else if map_flags.contains(MmapFlags::HUGE) {
        PageSize::Size2M
    } else {
        PageSize::Size4K
    };

    let start = addr.align_down(page_size);
    let end = (addr + length).align_up(page_size);
    let mut length = end - start;

    let start = if map_flags.intersects(MmapFlags::FIXED | MmapFlags::FIXED_NOREPLACE) {
        let dst_addr = VirtAddr::from(start);
        if !map_flags.contains(MmapFlags::FIXED_NOREPLACE) {
            aspace.unmap(dst_addr, length)?;
        }
        dst_addr
    } else {
        let align = page_size as usize;
        aspace
            .find_free_area(
                VirtAddr::from(start),
                length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
                align,
            )
            .or(aspace.find_free_area(
                aspace.base(),
                length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
                align,
            ))
            .ok_or(AxError::NoMemory)?
    };

    let file = if fd > 0 {
        Some(get_file_like(fd)?)
    } else {
        None
    };

    let backend = match map_type {
        MmapFlags::SHARED | MmapFlags::SHARED_VALIDATE => {
            if let Some(ref file) = file {
                // Try device mmap first (ExportedGemBuffer, etc.)
                if let Ok(device_mmap) = file.device_mmap(offset as u64) {
                    match device_mmap {
                        DeviceMmap::Physical(mut range) => {
                            range.start += offset;
                            if range.is_empty() {
                                return Err(AxError::InvalidInput);
                            }
                            length = length.min(range.size().align_down(page_size));
                            Backend::new_linear(
                                start.as_usize() as isize - range.start.as_usize() as isize,
                            );
                        }
                        DeviceMmap::None => return Err(AxError::NoSuchDevice),
                        _ => return Err(AxError::InvalidInput),
                    }
                }

                // Fall through to file-backed mmap
                let (backend, flags) = file.file_mmap()?;
                // man 2 mmap EACCES: a file mapping requires the fd to be
                // open for reading, and MAP_SHARED+PROT_WRITE additionally
                // requires the fd to be open for writing.
                if !flags.contains(FileFlags::READ) {
                    return Err(AxError::PermissionDenied);
                }
                if permission_flags.contains(MmapProt::WRITE) && !flags.contains(FileFlags::WRITE) {
                    return Err(AxError::PermissionDenied);
                }
                match backend.clone() {
                    FileBackend::Cached(cache) => {
                        // TODO(mivik): file mmap page size
                        Backend::new_file(
                            start,
                            cache,
                            flags,
                            offset,
                            &curr.as_thread().proc_data.aspace,
                        )
                    }
                    FileBackend::Direct(loc) => {
                        let device = loc
                            .entry()
                            .downcast::<Device>()
                            .map_err(|_| AxError::NoSuchDevice)?;

                        match device.mmap(offset as u64) {
                            DeviceMmap::None => {
                                return Err(AxError::NoSuchDevice);
                            }
                            DeviceMmap::ReadOnly => {
                                Backend::new_cow(start, page_size, backend, offset as u64, None)
                            }
                            DeviceMmap::Physical(range) => {
                                if range.is_empty() {
                                    return Err(AxError::InvalidInput);
                                }
                                length = capped_device_map_len(length, range.size(), page_size);
                                Backend::new_linear(
                                    start.as_usize() as isize - range.start.as_usize() as isize,
                                )
                            }
                            DeviceMmap::Cache(cache) => Backend::new_file(
                                start,
                                cache,
                                flags,
                                offset,
                                &curr.as_thread().proc_data.aspace,
                            ),
                        }
                    }
                }
            } else {
                Backend::new_shared(start, Arc::new(SharedPages::new(length, PageSize::Size4K)?))
            }
        }
        MmapFlags::PRIVATE => {
            if let Some(ref file) = file {
                // Private file-backed mmap
                let (backend, file_flags) = file.file_mmap()?;
                // man 2 mmap EACCES: a file mapping requires the fd to be
                // open for reading (MAP_PRIVATE still page-faults from file
                // on initial access even when later writes are CoW).
                if !file_flags.contains(FileFlags::READ) {
                    return Err(AxError::PermissionDenied);
                }
                Backend::new_cow(start, page_size, backend, offset as u64, None)
            } else {
                Backend::new_alloc(start, page_size)
            }
        }
        _ => return Err(AxError::InvalidInput),
    };

    let populate = map_flags.contains(MmapFlags::POPULATE);
    aspace.map(start, length, permission_flags.into(), populate, backend)?;

    Ok(start.as_usize() as _)
}

pub fn sys_munmap(addr: usize, length: usize) -> AxResult<isize> {
    // man 2 munmap: "length was 0" → EINVAL (since Linux 2.6.12).
    if length == 0 {
        return Err(AxError::InvalidInput);
    }
    debug!("sys_munmap <= addr: {addr:#x}, length: {length:x}");
    let curr = current();
    let mut aspace = curr.as_thread().proc_data.aspace.lock();
    let length = align_up_4k(length);
    let start_addr = VirtAddr::from(addr);
    aspace.unmap(start_addr, length)?;
    Ok(0)
}

pub fn sys_mprotect(addr: usize, length: usize, prot: u32) -> AxResult<isize> {
    // TODO: implement PROT_GROWSUP & PROT_GROWSDOWN
    let Some(permission_flags) = MmapProt::from_bits(prot) else {
        return Err(AxError::InvalidInput);
    };
    debug!("sys_mprotect <= addr: {addr:#x}, length: {length:x}, prot: {permission_flags:?}");

    if permission_flags.contains(MmapProt::GROWDOWN | MmapProt::GROWSUP) {
        return Err(AxError::InvalidInput);
    }

    // man 2 mprotect: addr is not a multiple of page size → EINVAL.
    if !PageSize::Size4K.is_aligned(addr) {
        return Err(AxError::InvalidInput);
    }
    // length=0 is a no-op success on Linux.
    if length == 0 {
        return Ok(0);
    }

    let curr = current();
    let mut aspace = curr.as_thread().proc_data.aspace.lock();
    let length = align_up_4k(length);
    let start_addr = VirtAddr::from(addr);
    // man 2 mprotect: addresses without a mapping → ENOMEM.
    if aspace.find_area(start_addr).is_none() {
        return Err(AxError::NoMemory);
    }
    aspace.protect(start_addr, length, permission_flags.into())?;

    Ok(0)
}

pub fn sys_mremap(addr: usize, old_size: usize, new_size: usize, flags: u32) -> AxResult<isize> {
    debug!(
        "sys_mremap <= addr: {addr:#x}, old_size: {old_size:x}, new_size: {new_size:x}, flags: \
         {flags:#x}"
    );

    // TODO: full implementation

    if !addr.is_multiple_of(PageSize::Size4K as usize) {
        return Err(AxError::InvalidInput);
    }
    let addr = VirtAddr::from(addr);

    let curr = current();
    let aspace = curr.as_thread().proc_data.aspace.lock();
    let old_size = align_up_4k(old_size);
    let new_size = align_up_4k(new_size);

    let area = aspace.find_area(addr).ok_or(AxError::NoMemory)?;
    let flags = area.flags();
    // Determine the sharing type from the backend: Shared/File backends are
    // MAP_SHARED, Cow/Linear backends are MAP_PRIVATE.
    let mmap_flags = match area.backend() {
        Backend::Shared(_) | Backend::File(_) => MmapFlags::SHARED | MmapFlags::ANONYMOUS,
        Backend::Cow(_) | Backend::Linear(_) => MmapFlags::PRIVATE | MmapFlags::ANONYMOUS,
    };
    drop(aspace);
    let new_addr = sys_mmap(
        addr.as_usize(),
        new_size,
        flags.bits() as _,
        mmap_flags.bits(),
        -1,
        0,
    )? as usize;

    let copy_len = new_size.min(old_size);
    let data = vm_load(addr.as_ptr(), copy_len)?;
    vm_write_slice(new_addr as *mut u8, &data)?;

    sys_munmap(addr.as_usize(), old_size)?;

    Ok(new_addr as isize)
}

pub fn sys_madvise(addr: usize, length: usize, advice: i32) -> AxResult<isize> {
    debug!("sys_madvise <= addr: {addr:#x}, length: {length:x}, advice: {advice:#x}");
    Ok(0)
}

pub fn sys_msync(addr: usize, length: usize, flags: u32) -> AxResult<isize> {
    debug!("sys_msync <= addr: {addr:#x}, length: {length:x}, flags: {flags:#x}");

    Ok(0)
}

pub fn sys_mlock(addr: usize, length: usize) -> AxResult<isize> {
    sys_mlock2(addr, length, 0)
}

pub fn sys_mlock2(_addr: usize, _length: usize, _flags: u32) -> AxResult<isize> {
    Ok(0)
}
