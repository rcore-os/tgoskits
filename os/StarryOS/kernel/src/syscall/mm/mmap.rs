use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use ax_fs::FileBackend;
use ax_hal::paging::{MappingFlags, PageSize};
use ax_memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange, align_up_4k};
use ax_task::current;
use linux_raw_sys::general::*;

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
                let (backend, _) = file.file_mmap()?;
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

const MREMAP_VALID_FLAGS: u32 = MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP;

fn find_free(aspace: &crate::mm::AddrSpace, hint: VirtAddr, size: usize) -> AxResult<VirtAddr> {
    let limit = VirtAddrRange::new(aspace.base(), aspace.end());
    let align = PageSize::Size4K as usize;
    aspace
        .find_free_area(hint, size, limit, align)
        .or_else(|| aspace.find_free_area(aspace.base(), size, limit, align))
        .ok_or(AxError::NoMemory)
}

/// Copy data from `src` to `target`, then handle the source.
/// If `dontunmap`, the source is replaced with a faulting anonymous mapping.
fn mremap_move(
    aspace: &mut crate::mm::AddrSpace,
    src: VirtAddr,
    src_size: usize,
    target: VirtAddr,
    target_size: usize,
    flags: MappingFlags,
    dontunmap: bool,
) -> AxResult {
    let copy_size = src_size.min(target_size);
    let data = aspace.read_to_vec(src, copy_size)?;

    let backend = Backend::new_alloc(target, PageSize::Size4K);
    aspace.map(target, target_size, flags, true, backend)?;
    if let Err(e) = aspace.write(target, &data) {
        let _ = aspace.unmap(target, target_size);
        return Err(e);
    }

    let cleanup = |aspace: &mut crate::mm::AddrSpace| {
        let _ = aspace.unmap(target, target_size);
    };

    if let Err(e) = aspace.unmap(src, src_size) {
        cleanup(aspace);
        return Err(e);
    }
    if dontunmap {
        let empty = Backend::new_alloc(src, PageSize::Size4K);
        if let Err(e) = aspace.map(src, src_size, flags, false, empty) {
            cleanup(aspace);
            return Err(e);
        }
    }

    Ok(())
}

pub fn sys_mremap(
    addr: usize,
    old_size: usize,
    new_size: usize,
    flags: u32,
    new_addr: usize,
) -> AxResult<isize> {
    debug!(
        "sys_mremap <= addr: {addr:#x}, old_size: {old_size:x}, new_size: {new_size:x}, flags: \
         {flags:#x}, new_addr: {new_addr:#x}"
    );

    if new_size == 0 {
        return Err(AxError::InvalidInput);
    }
    if !addr.is_multiple_of(PageSize::Size4K as usize) {
        return Err(AxError::InvalidInput);
    }
    if flags & !MREMAP_VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    let addr = VirtAddr::from(addr);
    let old_size = align_up_4k(old_size);
    let new_size = align_up_4k(new_size);
    let may_move = flags & MREMAP_MAYMOVE != 0;
    let fixed = flags & MREMAP_FIXED != 0;
    let dontunmap = flags & MREMAP_DONTUNMAP != 0;

    // MREMAP_FIXED and MREMAP_DONTUNMAP both require MREMAP_MAYMOVE.
    if (fixed || dontunmap) && !may_move {
        return Err(AxError::InvalidInput);
    }
    // MREMAP_DONTUNMAP requires old_size == new_size.
    if dontunmap && old_size != new_size {
        return Err(AxError::InvalidInput);
    }
    // Validate new_addr for MREMAP_FIXED.
    if fixed {
        if !new_addr.is_multiple_of(PageSize::Size4K as usize) {
            return Err(AxError::InvalidInput);
        }
        // Old and new ranges must not overlap.
        let old_end = addr.as_usize() + old_size;
        let new_end = new_addr + new_size;
        if old_end > new_addr && new_end > addr.as_usize() {
            return Err(AxError::InvalidInput);
        }
    }

    let curr = current();
    let mut aspace = curr.as_thread().proc_data.aspace.lock();

    // Extract VMA properties into locals to release the immutable borrow
    // before any mutable operations.
    let (vma_end, vma_flags, is_private_anon, shared_pages) = {
        let area = aspace.find_area(addr).ok_or(AxError::BadAddress)?;
        let is_private_anon = matches!(area.backend(), Backend::Cow(cb) if cb.is_anonymous());
        let shared_pages = match area.backend() {
            Backend::Shared(sb) => Some(sb.pages().clone()),
            _ => None,
        };
        (area.end(), area.flags(), is_private_anon, shared_pages)
    };

    // MREMAP_DONTUNMAP is only valid for private anonymous mappings.
    if dontunmap && !is_private_anon {
        return Err(AxError::InvalidInput);
    }

    // old_size == 0 special case: only valid for shared mappings with MAYMOVE.
    if old_size == 0 {
        if shared_pages.is_none() || !may_move {
            return Err(AxError::InvalidInput);
        }
        let pages = shared_pages.unwrap();
        // Clamp new_size to the shared allocation size to avoid
        // creating a VMA larger than the backing SharedPages.
        let page_size = PageSize::Size4K as usize;
        let shared_size = pages.len() * page_size;
        let dup_size = new_size.min(shared_size);

        let target = if fixed {
            aspace.unmap(VirtAddr::from(new_addr), dup_size)?;
            VirtAddr::from(new_addr)
        } else {
            find_free(&aspace, addr, dup_size)?
        };
        let backend = Backend::new_shared(target, pages);
        aspace.map(target, dup_size, vma_flags, false, backend)?;
        return Ok(target.as_usize() as isize);
    }

    // Validate that old_size fits within the VMA.
    if addr + old_size > vma_end {
        return Err(AxError::BadAddress);
    }

    if fixed {
        let target = VirtAddr::from(new_addr);
        aspace.unmap(target, new_size)?;
        mremap_move(
            &mut aspace,
            addr,
            old_size,
            target,
            new_size,
            vma_flags,
            dontunmap,
        )?;
        return Ok(target.as_usize() as isize);
    }

    if new_size == old_size && !dontunmap {
        return Ok(addr.as_usize() as isize);
    }

    if new_size < old_size {
        aspace.unmap(addr + new_size, old_size - new_size)?;
        return Ok(addr.as_usize() as isize);
    }

    if dontunmap {
        let target = find_free(&aspace, addr + old_size, new_size)?;
        mremap_move(
            &mut aspace,
            addr,
            old_size,
            target,
            new_size,
            vma_flags,
            true,
        )?;
        return Ok(target.as_usize() as isize);
    }

    let delta = new_size - old_size;

    // Try in-place growth: extend the existing VMA rather than creating
    // a separate adjacent mapping. This preserves a single contiguous VMA.
    // Only fall through to move if the space is occupied or out of range;
    // propagate other errors (e.g., OOM from backend.map).
    if addr + old_size == vma_end {
        match aspace.extend_area(addr, delta) {
            Ok(()) => return Ok(addr.as_usize() as isize),
            Err(AxError::NoMemory | AxError::AlreadyExists) => {}
            Err(e) => return Err(e),
        }
    }

    // Can't grow in-place.
    if !may_move {
        return Err(AxError::NoMemory);
    }

    // Move: allocate new region, copy data, free old.
    let target = find_free(&aspace, addr + old_size, new_size)?;
    mremap_move(
        &mut aspace,
        addr,
        old_size,
        target,
        new_size,
        vma_flags,
        false,
    )?;
    Ok(target.as_usize() as isize)
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
