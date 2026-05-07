use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use ax_fs::{FileBackend, FileFlags};
use ax_hal::paging::{MappingFlags, PageSize};
use ax_memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange, align_up_4k};
use ax_task::current;
use linux_raw_sys::general::*;

use crate::{
    file::get_file_like,
    mm::{Backend, BackendOps, SharedPages},
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
                match file.device_mmap(offset as u64) {
                    Ok(DeviceMmap::Physical(mut range)) => {
                        range.start += offset;
                        if range.is_empty() {
                            return Err(AxError::InvalidInput);
                        }
                        length = length.min(range.size().align_down(page_size));
                        Backend::new_linear(
                            start,
                            start.as_usize() as isize - range.start.as_usize() as isize,
                            true,
                        )
                    }
                    Ok(DeviceMmap::None) => return Err(AxError::NoSuchDevice),
                    Ok(_) => return Err(AxError::InvalidInput),
                    Err(_) => {
                        // Fall through to file-backed mmap
                        let (backend, flags) = file.file_mmap()?;
                        // man 2 mmap EACCES: a file mapping requires the fd to be
                        // open for reading, and MAP_SHARED+PROT_WRITE additionally
                        // requires the fd to be open for writing.
                        if !flags.contains(FileFlags::READ) {
                            return Err(AxError::PermissionDenied);
                        }
                        if permission_flags.contains(MmapProt::WRITE)
                            && !flags.contains(FileFlags::WRITE)
                        {
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
                                    true,
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
                                    DeviceMmap::Physical(range) => {
                                        if range.is_empty() {
                                            return Err(AxError::InvalidInput);
                                        }
                                        length =
                                            capped_device_map_len(length, range.size(), page_size);
                                        Backend::new_linear(
                                            start,
                                            start.as_usize() as isize
                                                - range.start.as_usize() as isize,
                                            true,
                                        )
                                    }
                                    DeviceMmap::Cache(cache) => Backend::new_file(
                                        start,
                                        cache,
                                        flags,
                                        offset,
                                        &curr.as_thread().proc_data.aspace,
                                        true,
                                    ),
                                }
                            }
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
                Backend::new_cow(start, page_size, backend, offset as u64, None, false)
            } else {
                Backend::new_alloc(start, page_size, "")
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

const MREMAP_VALID_FLAGS: u32 = MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP;

fn find_free(
    aspace: &crate::mm::AddrSpace,
    hint: VirtAddr,
    size: usize,
    align: usize,
) -> AxResult<VirtAddr> {
    let limit = VirtAddrRange::new(aspace.base(), aspace.end());
    aspace
        .find_free_area(hint, size, limit, align)
        .or_else(|| aspace.find_free_area(aspace.base(), size, limit, align))
        .ok_or(AxError::NoMemory)
}

struct MremapMove<'a> {
    src: VirtAddr,
    src_size: usize,
    target: VirtAddr,
    target_size: usize,
    src_backend: &'a Backend,
    flags: MappingFlags,
    dontunmap: bool,
    src_offset: usize,
}

fn mremap_move(
    aspace: &mut crate::mm::AddrSpace,
    aspace_ref: &Arc<ax_sync::Mutex<crate::mm::AddrSpace>>,
    move_args: MremapMove<'_>,
) -> AxResult {
    let MremapMove {
        src,
        src_size,
        target,
        target_size,
        src_backend,
        flags,
        dontunmap,
        src_offset,
    } = move_args;
    let move_size = src_size.min(target_size);
    let backend = src_backend.relocated(target, src_offset, aspace_ref)?;

    aspace.map(target, target_size, flags, false, backend)?;

    if dontunmap {
        let empty = Backend::new_alloc(src, src_backend.page_size(), "");
        if let Err(e) = aspace.replace_area_metadata(src, move_size, flags, empty) {
            let _ = aspace.unmap(target, target_size);
            return Err(e);
        }
    }

    if let Err(e) = aspace.move_pages(src, target, move_size) {
        if dontunmap {
            aspace
                .replace_area_metadata(src, move_size, flags, src_backend.clone())
                .expect("restore source VMA metadata after failed mremap move");
        }
        let _ = aspace.unmap(target, target_size);
        return Err(e);
    }

    if dontunmap {
        return Ok(());
    }

    aspace
        .unmap_metadata(src, move_size)
        .expect("remove moved source VMA metadata");

    if src_size > move_size {
        aspace
            .unmap(src + move_size, src_size - move_size)
            .expect("unmap truncated source tail after mremap move");
    } else {
        debug_assert_eq!(src_size, move_size);
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
    if flags & !MREMAP_VALID_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    let addr = VirtAddr::from(addr);
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

    let (vma_start, vma_end, vma_flags, src_backend, shared_pages, page_size) = {
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
            area.backend().page_size(),
        )
    };
    if !page_size.is_aligned(addr.as_usize()) {
        return Err(AxError::InvalidInput);
    }
    let old_size = old_size.align_up(page_size);
    let new_size = new_size.align_up(page_size);
    let src_offset = addr - vma_start;

    if dontunmap && !matches!(&src_backend, Backend::Cow(cow) if cow.is_anonymous()) {
        return Err(AxError::InvalidInput);
    }

    // old_size == 0: duplicate a shared mapping (Linux special case).
    if old_size == 0 {
        if shared_pages.is_none() || !may_move {
            return Err(AxError::InvalidInput);
        }
        let pages = shared_pages.unwrap();
        let shared_size = pages.len() * pages.size as usize;
        if src_offset + new_size > shared_size {
            return Err(AxError::InvalidInput);
        }

        let target = if fixed {
            if !page_size.is_aligned(new_addr) {
                return Err(AxError::InvalidInput);
            }
            aspace.unmap(VirtAddr::from(new_addr), new_size)?;
            VirtAddr::from(new_addr)
        } else {
            find_free(&aspace, addr, new_size, page_size as usize)?
        };
        let backend_start = target
            .as_usize()
            .checked_sub(src_offset)
            .map(VirtAddr::from)
            .ok_or(AxError::InvalidInput)?;
        let backend = Backend::new_shared(backend_start, pages);
        aspace.map(target, new_size, vma_flags, false, backend)?;
        return Ok(target.as_usize() as isize);
    }

    let old_end = addr
        .as_usize()
        .checked_add(old_size)
        .map(VirtAddr::from)
        .ok_or(AxError::InvalidInput)?;
    if old_end > vma_end {
        return Err(AxError::BadAddress);
    }

    if fixed {
        if !page_size.is_aligned(new_addr) {
            return Err(AxError::InvalidInput);
        }
        let target = VirtAddr::from(new_addr);
        aspace.unmap(target, new_size)?;

        mremap_move(
            &mut aspace,
            aspace_ref,
            MremapMove {
                src: addr,
                src_size: old_size,
                target,
                target_size: new_size,
                src_backend: &src_backend,
                flags: vma_flags,
                dontunmap,
                src_offset,
            },
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
        let target = find_free(&aspace, addr + old_size, new_size, page_size as usize)?;
        mremap_move(
            &mut aspace,
            aspace_ref,
            MremapMove {
                src: addr,
                src_size: old_size,
                target,
                target_size: new_size,
                src_backend: &src_backend,
                flags: vma_flags,
                dontunmap: true,
                src_offset,
            },
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

    let target = find_free(&aspace, addr + old_size, new_size, page_size as usize)?;
    mremap_move(
        &mut aspace,
        aspace_ref,
        MremapMove {
            src: addr,
            src_size: old_size,
            target,
            target_size: new_size,
            src_backend: &src_backend,
            flags: vma_flags,
            dontunmap: false,
            src_offset,
        },
    )?;
    Ok(target.as_usize() as isize)
}

pub fn sys_madvise(addr: usize, length: usize, advice: i32) -> AxResult<isize> {
    debug!("sys_madvise <= addr: {addr:#x}, length: {length:x}, advice: {advice:#x}");

    match advice as u32 {
        MADV_NORMAL | MADV_RANDOM | MADV_SEQUENTIAL | MADV_WILLNEED | MADV_DONTNEED | MADV_FREE
        | MADV_REMOVE | MADV_DONTFORK | MADV_DOFORK | MADV_MERGEABLE | MADV_UNMERGEABLE
        | MADV_HUGEPAGE | MADV_NOHUGEPAGE | MADV_DONTDUMP | MADV_DODUMP | MADV_WIPEONFORK
        | MADV_KEEPONFORK | MADV_COLD | MADV_PAGEOUT | MADV_POPULATE_READ | MADV_POPULATE_WRITE
        | MADV_DONTNEED_LOCKED | MADV_COLLAPSE | MADV_HWPOISON | MADV_SOFT_OFFLINE => {}
        _ => return Err(AxError::InvalidInput),
    }

    if !addr.is_multiple_of(PageSize::Size4K as usize) {
        return Err(AxError::InvalidInput);
    }

    if length > 0 {
        let curr = current();
        let aspace = curr.as_thread().proc_data.aspace.lock();
        if aspace.find_area(VirtAddr::from(addr)).is_none() {
            return Err(AxError::NoMemory);
        }
    }

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
