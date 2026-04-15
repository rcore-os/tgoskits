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

fn mremap_move(
    aspace: &mut crate::mm::AddrSpace,
    aspace_ref: &Arc<ax_sync::Mutex<crate::mm::AddrSpace>>,
    src: VirtAddr,
    src_size: usize,
    target: VirtAddr,
    target_size: usize,
    src_backend: &Backend,
    flags: MappingFlags,
    dontunmap: bool,
    src_offset: usize,
) -> AxResult {
    let backend = src_backend.relocated(target, src_offset, aspace_ref);
    aspace.map(target, target_size, flags, false, backend)?;

    let move_size = src_size.min(target_size);
    aspace.move_pages(src, target, move_size);

    if let Err(e) = aspace.unmap(src, src_size) {
        let _ = aspace.unmap(target, target_size);
        return Err(e);
    }

    if dontunmap {
        let empty = Backend::new_alloc(src, PageSize::Size4K);
        if let Err(e) = aspace.map(src, src_size, flags, false, empty) {
            let _ = aspace.unmap(target, target_size);
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

    if (fixed || dontunmap) && !may_move {
        return Err(AxError::InvalidInput);
    }
    if dontunmap && old_size != new_size {
        return Err(AxError::InvalidInput);
    }
    if fixed {
        if !new_addr.is_multiple_of(PageSize::Size4K as usize) {
            return Err(AxError::InvalidInput);
        }
        let old_end = addr
            .as_usize()
            .checked_add(old_size)
            .ok_or(AxError::InvalidInput)?;
        let new_end = new_addr
            .checked_add(new_size)
            .ok_or(AxError::InvalidInput)?;
        if old_end > new_addr && new_end > addr.as_usize() {
            return Err(AxError::InvalidInput);
        }
    }

    let curr = current();
    let aspace_ref = &curr.as_thread().proc_data.aspace;
    let mut aspace = aspace_ref.lock();

    let (vma_start, vma_end, vma_flags, src_backend, shared_pages) = {
        let area = aspace.find_area(addr).ok_or(AxError::BadAddress)?;
        let shared_pages = match area.backend() {
            Backend::Shared(sb) => Some(sb.pages().clone()),
            _ => None,
        };
        (
            area.start(),
            area.end(),
            area.flags(),
            area.backend().clone(),
            shared_pages,
        )
    };

    // DONTUNMAP only for Cow and Shared (Linux 5.13+ relaxed this from
    // private-anonymous-only to exclude VM_DONTEXPAND/VM_MIXEDMAP).
    if dontunmap && !matches!(src_backend, Backend::Cow(_) | Backend::Shared(_)) {
        return Err(AxError::InvalidInput);
    }

    // old_size == 0: duplicate a shared mapping (Linux special case).
    if old_size == 0 {
        if shared_pages.is_none() || !may_move {
            return Err(AxError::InvalidInput);
        }
        let pages = shared_pages.unwrap();
        let shared_size = pages.len() * pages.size as usize;
        let dup_size = new_size.min(shared_size);

        let target = if fixed {
            aspace.unmap(VirtAddr::from(new_addr), new_size)?;
            VirtAddr::from(new_addr)
        } else {
            find_free(&aspace, addr, dup_size)?
        };
        let backend = Backend::new_shared(target, pages);
        aspace.map(target, dup_size, vma_flags, false, backend)?;
        return Ok(target.as_usize() as isize);
    }

    if addr + old_size > vma_end {
        // Multi-VMA: only allowed for FIXED + same-size (Linux 6.17+).
        if !fixed || old_size != new_size {
            return Err(AxError::BadAddress);
        }
    }

    let src_offset = addr - vma_start;

    if fixed {
        let target = VirtAddr::from(new_addr);
        aspace.unmap(target, new_size)?;

        if old_size == new_size && addr + old_size > vma_end {
            // Multi-VMA move: collect all fragments in [addr, addr+old_size).
            let fragments = aspace.areas_in_range(addr, old_size);
            if fragments.is_empty() {
                return Err(AxError::BadAddress);
            }
            for (frag_start, frag_size, frag_flags, frag_backend) in &fragments {
                let offset_in_range = *frag_start - addr;
                let frag_target = target + offset_in_range;
                let frag_vma_start = aspace
                    .find_area(*frag_start)
                    .expect("fragment must belong to a VMA")
                    .start();
                let frag_src_offset = *frag_start - frag_vma_start;

                let backend = frag_backend.relocated(frag_target, frag_src_offset, aspace_ref);
                aspace.map(frag_target, *frag_size, *frag_flags, false, backend)?;
                aspace.move_pages(*frag_start, frag_target, *frag_size);
            }
            aspace.unmap(addr, old_size)?;
            if dontunmap {
                let empty = Backend::new_alloc(addr, PageSize::Size4K);
                aspace.map(addr, old_size, vma_flags, false, empty)?;
            }
            return Ok(target.as_usize() as isize);
        }

        mremap_move(
            &mut aspace,
            aspace_ref,
            addr,
            old_size,
            target,
            new_size,
            &src_backend,
            vma_flags,
            dontunmap,
            src_offset,
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
            aspace_ref,
            addr,
            old_size,
            target,
            new_size,
            &src_backend,
            vma_flags,
            true,
            src_offset,
        )?;
        return Ok(target.as_usize() as isize);
    }

    let delta = new_size - old_size;

    if addr + old_size == vma_end {
        match aspace.extend_area(addr, delta) {
            Ok(()) => return Ok(addr.as_usize() as isize),
            Err(AxError::NoMemory | AxError::AlreadyExists) => {}
            Err(e) => return Err(e),
        }
    }

    if !may_move {
        return Err(AxError::NoMemory);
    }

    let target = find_free(&aspace, addr + old_size, new_size)?;
    mremap_move(
        &mut aspace,
        aspace_ref,
        addr,
        old_size,
        target,
        new_size,
        &src_backend,
        vma_flags,
        false,
        src_offset,
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
