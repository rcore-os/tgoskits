use alloc::sync::Arc;

use axerrno::{AxError, AxResult};
use axfs_ng::FileBackend;
use axhal::paging::{MappingFlags, PageSize};
use axmm::backend::{Backend, SharedPages};
use axtask::current;
use linux_raw_sys::general::*;
use memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange, align_up_4k};
use starry_core::{
    task::AsThread,
    vfs::{Device, DeviceMmap},
};
use starry_vm::{vm_load, vm_write_slice};

use crate::file::{File, FileLike};

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
    let map_type = map_flags & MmapFlags::TYPE;
    if !matches!(
        map_type,
        MmapFlags::PRIVATE | MmapFlags::SHARED | MmapFlags::SHARED_VALIDATE
    ) {
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
        aspace
            .find_free_area(
                VirtAddr::from(start),
                length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
            )
            .or(aspace.find_free_area(
                aspace.base(),
                length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
            ))
            .ok_or(AxError::NoMemory)?
    };

    let file = if fd > 0 {
        Some(File::from_fd(fd)?)
    } else {
        None
    };

    let backend = match map_type {
        MmapFlags::SHARED | MmapFlags::SHARED_VALIDATE => {
            if let Some(file) = file {
                let file = file.inner();
                let backend = file.backend()?.clone();
                match file.backend()?.clone() {
                    FileBackend::Cached(cache) => {
                        // TODO(mivik): file mmap page size
                        Backend::new_file(
                            start,
                            cache,
                            file.flags(),
                            offset,
                            &curr.as_thread().proc_data.aspace,
                        )
                    }
                    FileBackend::Direct(loc) => {
                        let device = loc
                            .entry()
                            .downcast::<Device>()
                            .map_err(|_| AxError::NoSuchDevice)?;

                        match device.mmap() {
                            DeviceMmap::None => {
                                return Err(AxError::NoSuchDevice);
                            }
                            DeviceMmap::ReadOnly => {
                                Backend::new_cow(start, page_size, backend, offset as u64, None)
                            }
                            DeviceMmap::Physical(mut range) => {
                                range.start += offset;
                                if range.is_empty() {
                                    return Err(AxError::InvalidInput);
                                }
                                length = length.min(range.size().align_down(page_size));
                                Backend::new_linear(
                                    start.as_usize() as isize - range.start.as_usize() as isize,
                                )
                            }
                            DeviceMmap::Cache(cache) => Backend::new_file(
                                start,
                                cache,
                                file.flags(),
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
            if let Some(file) = file {
                // Private mapping from a file
                let backend = file.inner().backend()?.clone();
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

    let curr = current();
    let mut aspace = curr.as_thread().proc_data.aspace.lock();
    let length = align_up_4k(length);
    let start_addr = VirtAddr::from(addr);
    aspace.protect(start_addr, length, permission_flags.into())?;

    Ok(0)
}

pub fn sys_mremap(addr: usize, old_size: usize, new_size: usize, flags: u32) -> AxResult<isize> {
    debug!(
        "sys_mremap <= addr: {addr:#x}, old_size: {old_size:x}, new_size: {new_size:x}, flags: \
         {flags:#x}"
    );

    // TODO: full implementation

    if addr % PageSize::Size4K as usize != 0 {
        return Err(AxError::InvalidInput);
    }
    let addr = VirtAddr::from(addr);

    let curr = current();
    let aspace = curr.as_thread().proc_data.aspace.lock();
    let old_size = align_up_4k(old_size);
    let new_size = align_up_4k(new_size);

    let flags = aspace.find_area(addr).ok_or(AxError::NoMemory)?.flags();
    drop(aspace);
    let new_addr = sys_mmap(
        addr.as_usize(),
        new_size,
        flags.bits() as _,
        MmapFlags::PRIVATE.bits(),
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
