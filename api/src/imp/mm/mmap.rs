use alloc::vec;
use axerrno::{LinuxError, LinuxResult};
use axhal::paging::{MappingFlags, PageSize};
use axtask::current;
use linux_raw_sys::general::*;
use memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange, align_up_4k};
use starry_core::task::StarryTaskExt;

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
    #[derive(Debug, PartialEq, Eq)]
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
        /// Don't check for reservations.
        const NORESERVE = MAP_NORESERVE;
        /// Allocation is for a stack.
        const STACK = MAP_STACK;
        /// Huge page
        const HUGE = MAP_HUGETLB;
        /// Huge page 1g size
        const HUGE_1GB = MAP_HUGETLB | MAP_HUGE_1GB;

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
) -> LinuxResult<isize> {
    if length == 0 {
        return Err(LinuxError::EINVAL);
    }

    let curr = current();
    let mut aspace = StarryTaskExt::of(&curr).process_data().aspace.lock();
    let permission_flags = MmapProt::from_bits_truncate(prot);
    // TODO: check illegal flags for mmap
    // An example is the flags contained none of MAP_PRIVATE, MAP_SHARED, or MAP_SHARED_VALIDATE.
    let map_flags = MmapFlags::from_bits_truncate(flags);
    if map_flags.contains(MmapFlags::PRIVATE | MmapFlags::SHARED) {
        return Err(LinuxError::EINVAL);
    }

    info!(
        "sys_mmap: addr: {:x?}, length: {:x?}, prot: {:?}, flags: {:?}, fd: {:?}, offset: {:?}",
        addr, length, permission_flags, map_flags, fd, offset
    );

    // let page_size = if map_flags.contains(MmapFlags::HUGE_1GB) {
    //     PageSize::Size1G
    // } else if map_flags.contains(MmapFlags::HUGE) {
    //     PageSize::Size2M
    // } else {
    //     PageSize::Size4K
    // };
    let page_size = PageSize::Size4K;

    let start = addr.align_down(page_size);
    let end = (addr + length).align_up(page_size);
    let aligned_length = end - start;
    debug!(
        "start: {:x?}, end: {:x?}, aligned_length: {:x?}",
        start, end, aligned_length
    );

    let start_addr = if map_flags.intersects(MmapFlags::FIXED | MmapFlags::FIXED_NOREPLACE) {
        let dst_addr = VirtAddr::from(start);
        if !map_flags.contains(MmapFlags::FIXED_NOREPLACE) {
            aspace.unmap(dst_addr, aligned_length)?;
        }
        dst_addr
    } else {
        aspace
            .find_free_area(
                VirtAddr::from(start),
                aligned_length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
                page_size,
            )
            .or(aspace.find_free_area(
                aspace.base(),
                aligned_length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
                page_size,
            ))
            .ok_or(LinuxError::ENOMEM)?
    };

    let populate = fd > 0 && !map_flags.contains(MmapFlags::ANONYMOUS);

    match map_flags & MmapFlags::TYPE {
        MmapFlags::SHARED | MmapFlags::SHARED_VALIDATE => {
            aspace.map_shared(
                start_addr,
                aligned_length,
                permission_flags.into(),
                None,
                page_size,
            )?;
        }
        MmapFlags::PRIVATE => {
            aspace.map_alloc(
                start_addr,
                aligned_length,
                permission_flags.into(),
                populate,
                page_size,
            )?;
        }
        _ => return Err(LinuxError::EINVAL),
    }

    if populate {
        if permission_flags.contains(MmapProt::WRITE) {
            warn!("sys_mmap: PROT_WRITE for a file mapping is not supported yet");
        }
        let file = File::from_fd(fd)?;
        let mut file = file.inner();
        let file_size = file.inner().len()? as usize;
        if offset < 0 || offset as usize >= file_size {
            return Err(LinuxError::EINVAL);
        }
        let offset = offset as usize;
        let length = core::cmp::min(length, file_size - offset);
        let mut buf = vec![0u8; length];
        file.read_at(&mut buf, offset as u64)?;
        aspace.write(start_addr, page_size, &buf)?;
    }
    Ok(start_addr.as_usize() as _)
}

pub fn sys_munmap(addr: usize, length: usize) -> LinuxResult<isize> {
    let curr = current();
    let mut aspace = StarryTaskExt::of(&curr).process_data().aspace.lock();
    let length = align_up_4k(length);
    let start_addr = VirtAddr::from(addr);
    aspace.unmap(start_addr, length)?;
    axhal::arch::flush_tlb(None);
    Ok(0)
}

pub fn sys_mprotect(addr: usize, length: usize, prot: u32) -> LinuxResult<isize> {
    // TODO: implement PROT_GROWSUP & PROT_GROWSDOWN
    let Some(permission_flags) = MmapProt::from_bits(prot) else {
        return Err(LinuxError::EINVAL);
    };
    if permission_flags.contains(MmapProt::GROWDOWN | MmapProt::GROWSUP) {
        return Err(LinuxError::EINVAL);
    }

    let curr = current();
    let mut aspace = StarryTaskExt::of(&curr).process_data().aspace.lock();
    let length = align_up_4k(length);
    let start_addr = VirtAddr::from(addr);
    // TODO: is 4k right here?
    aspace.protect(start_addr, length, permission_flags.into())?;

    Ok(0)
}

pub fn sys_madvise(addr: usize, length: usize, advice: i32) -> LinuxResult<isize> {
    debug!(
        "sys_madvise <= addr: {:#x}, length: {:x}, advice: {:#x}",
        addr, length, advice
    );
    Ok(0)
}

pub fn sys_msync(addr: usize, length: usize, flags: u32) -> LinuxResult<isize> {
    debug!(
        "sys_msync <= addr: {:#x}, length: {:x}, flags: {:#x}",
        addr, length, flags
    );

    Ok(0)
}
