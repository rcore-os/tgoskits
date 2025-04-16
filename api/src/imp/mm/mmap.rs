use alloc::vec;
use axerrno::{LinuxError, LinuxResult};
use axhal::paging::MappingFlags;
use axtask::{TaskExtRef, current};
use macro_rules_attribute::apply;
use memory_addr::{VirtAddr, VirtAddrRange};

use crate::{
    ptr::{PtrWrapper, UserPtr},
    syscall_instrument,
};

bitflags::bitflags! {
    /// permissions for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    #[derive(Debug)]
    struct MmapProt: i32 {
        /// Page can be read.
        const PROT_READ = 1 << 0;
        /// Page can be written.
        const PROT_WRITE = 1 << 1;
        /// Page can be executed.
        const PROT_EXEC = 1 << 2;
        /// Extend change to start of growsdown vma (mprotect only).
        const PROT_GROWDOWN = 0x01000000;
        /// Extend change to start of growsup vma (mprotect only).
        const PROT_GROWSUP = 0x02000000;
    }
}

impl From<MmapProt> for MappingFlags {
    fn from(value: MmapProt) -> Self {
        let mut flags = MappingFlags::USER;
        if value.contains(MmapProt::PROT_READ) {
            flags |= MappingFlags::READ;
        }
        if value.contains(MmapProt::PROT_WRITE) {
            flags |= MappingFlags::WRITE;
        }
        if value.contains(MmapProt::PROT_EXEC) {
            flags |= MappingFlags::EXECUTE;
        }
        flags
    }
}

bitflags::bitflags! {
    /// flags for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    #[derive(Debug)]
    struct MmapFlags: i32 {
        /// Share changes
        const MAP_SHARED = 1 << 0;
        /// Changes private; copy pages on write.
        const MAP_PRIVATE = 1 << 1;
        /// Map address must be exactly as requested, no matter whether it is available.
        const MAP_FIXED = 1 << 4;
        /// Don't use a file.
        const MAP_ANONYMOUS = 1 << 5;
        /// Don't check for reservations.
        const MAP_NORESERVE = 1 << 14;
        /// Allocation is for a stack.
        const MAP_STACK = 0x20000;
    }
}

#[apply(syscall_instrument)]
pub fn sys_mmap(
    addr: UserPtr<usize>,
    length: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: isize,
) -> LinuxResult<isize> {
    // Safety: addr is used for mapping, and we won't directly access it.
    let mut addr = unsafe { addr.into_inner() };

    let curr = current();
    let process_data = curr.task_ext().process_data();
    let mut aspace = process_data.aspace.lock();
    let permission_flags = MmapProt::from_bits_truncate(prot);
    // TODO: check illegal flags for mmap
    // An example is the flags contained none of MAP_PRIVATE, MAP_SHARED, or MAP_SHARED_VALIDATE.
    let map_flags = MmapFlags::from_bits_truncate(flags);
    let mut aligned_length = length;

    if addr.is_null() {
        aligned_length = memory_addr::align_up_4k(aligned_length);
    } else {
        let start = addr as usize;
        let mut end = start + aligned_length;
        addr = memory_addr::align_down_4k(start) as *mut usize;
        end = memory_addr::align_up_4k(end);
        aligned_length = end - start;
    }

    info!(
        "mmap: addr: {:?}, length: {:x?}, prot: {:?}, flags: {:?}, fd: {:?}, offset: {:?}",
        addr, length, permission_flags, map_flags, fd, offset
    );

    let start_addr = if map_flags.contains(MmapFlags::MAP_FIXED) {
        if addr.is_null() {
            return Err(LinuxError::EINVAL);
        }
        let dst_addr = VirtAddr::from(addr as usize);
        aspace.unmap(dst_addr, aligned_length)?;
        dst_addr
    } else {
        aspace
            .find_free_area(
                VirtAddr::from(addr as usize),
                aligned_length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
            )
            .or(aspace.find_free_area(
                aspace.base(),
                aligned_length,
                VirtAddrRange::new(aspace.base(), aspace.end()),
            ))
            .ok_or(LinuxError::ENOMEM)?
    };

    let populate = if fd == -1 {
        false
    } else {
        !map_flags.contains(MmapFlags::MAP_ANONYMOUS)
    };

    aspace.map_alloc(
        start_addr,
        aligned_length,
        permission_flags.into(),
        populate,
    )?;

    if populate {
        let file = arceos_posix_api::get_file_like(fd)?;
        let file_size = file.stat()?.st_size as usize;
        let file = file
            .into_any()
            .downcast::<arceos_posix_api::File>()
            .map_err(|_| LinuxError::EBADF)?;
        let file = file.inner().lock();
        if offset < 0 || offset as usize >= file_size {
            return Err(LinuxError::EINVAL);
        }
        let offset = offset as usize;
        let length = core::cmp::min(length, file_size - offset);
        let mut buf = vec![0u8; length];
        file.read_at(offset as u64, &mut buf)?;
        aspace.write(start_addr, &buf)?;
    }
    Ok(start_addr.as_usize() as _)
}

#[apply(syscall_instrument)]
pub fn sys_munmap(addr: UserPtr<usize>, length: usize) -> LinuxResult<isize> {
    // Safety: addr is used for mapping, and we won't directly access it.
    let addr = unsafe { addr.into_inner() };

    let curr = current();
    let process_data = curr.task_ext().process_data();
    let mut aspace = process_data.aspace.lock();
    let length = memory_addr::align_up_4k(length);
    let start_addr = VirtAddr::from(addr as usize);
    aspace.unmap(start_addr, length)?;
    axhal::arch::flush_tlb(None);
    Ok(0)
}

#[apply(syscall_instrument)]
pub fn sys_mprotect(addr: UserPtr<usize>, length: usize, prot: i32) -> LinuxResult<isize> {
    // Safety: addr is used for mapping, and we won't directly access it.
    let addr = unsafe { addr.into_inner() };

    // TODO: implement PROT_GROWSUP & PROT_GROWSDOWN
    let Some(permission_flags) = MmapProt::from_bits(prot) else {
        return Err(LinuxError::EINVAL);
    };
    if permission_flags.contains(MmapProt::PROT_GROWDOWN | MmapProt::PROT_GROWSUP) {
        return Err(LinuxError::EINVAL);
    }

    let curr = current();
    let process_data = curr.task_ext().process_data();
    let mut aspace = process_data.aspace.lock();
    let length = memory_addr::align_up_4k(length);
    let start_addr = VirtAddr::from(addr as usize);
    aspace.protect(start_addr, length, permission_flags.into())?;

    Ok(0)
}
