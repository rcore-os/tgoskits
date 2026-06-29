use alloc::{sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::{FileBackend, FileFlags};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, VirtAddr, VirtAddrRange, align_up_4k};
use ax_runtime::hal::paging::{MappingFlags, PageSize};
use ax_task::current;
use linux_raw_sys::general::*;

use crate::{
    file::get_file_like,
    mm::{Backend, BackendOps, SharedPages},
    pseudofs::{Device, DeviceMmap},
    syscall::fs::{memfd_check_write_seal, memfd_check_write_seal_for_shared_file_backend},
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
        let mut flags = MappingFlags::empty();
        if value.contains(MmapProt::READ) {
            flags |= MappingFlags::READ;
        }
        if value.contains(MmapProt::WRITE) {
            // Writable pages must also be readable. RISC-V's privileged spec
            // reserves the (R=0, W=1) PTE encoding, so a PROT_WRITE-only mmap
            // would produce an unusable PTE. Linux implicitly promotes
            // PROT_WRITE to PROT_READ | PROT_WRITE for this reason; match that
            // behavior so userspace paths that mmap with PROT_WRITE alone
            // (e.g. weston's drm-pixman shadow framebuffer) work on riscv64.
            flags |= MappingFlags::READ | MappingFlags::WRITE;
        }
        if value.contains(MmapProt::EXEC) {
            flags |= MappingFlags::EXECUTE;
        }
        // PROT_NONE must yield empty flags so the PTE is non-present and any
        // access faults. Tagging it USER would, on x86_64, still set the PRESENT
        // bit (present implies readable on x86) and silently defeat the
        // protection — breaking guard pages such as JVM thread-stack guards,
        // letting a stack overflow corrupt adjacent memory instead of trapping.
        // Only accessible mappings get the USER tag.
        if !flags.is_empty() {
            flags |= MappingFlags::USER;
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
        /// Synchronous file updates for persistent memory mappings.
        const SYNC = MAP_SYNC;
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
    let curr_aspace = curr.as_thread().proc_data.aspace();
    let mut aspace = curr_aspace.lock();
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
    if map_flags.contains(MmapFlags::SYNC) {
        return Err(AxError::OperationNotSupported);
    }
    let anonymous = map_flags.contains(MmapFlags::ANONYMOUS);
    let map_type = match flags & MmapFlags::TYPE.bits() {
        MAP_SHARED => MmapFlags::SHARED,
        MAP_SHARED_VALIDATE if !anonymous => MmapFlags::SHARED,
        MAP_PRIVATE => MmapFlags::PRIVATE,
        _ => return Err(AxError::InvalidInput),
    };
    let offset: usize = offset.try_into().map_err(|_| AxError::InvalidInput)?;
    if !PageSize::Size4K.is_aligned(offset) {
        return Err(AxError::InvalidInput);
    }
    if !anonymous && fd < 0 {
        return Err(AxError::BadFileDescriptor);
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

    let aligned = addr.align_down(page_size);
    // Guard against `addr + length` (and the page-size round-up that `align_up`
    // performs internally as `raw_end + page_size - 1`) wrapping past the address
    // space for pathological requests (e.g. MAP_FIXED with a near-max addr and a
    // huge length). Linux rejects these with EINVAL up front. `checked_add`
    // catches the first overflow; the `end < raw_end` check catches the case where
    // `addr + length` itself didn't overflow but rounding up to the page boundary
    // did — otherwise the wrapped value would flow into the length computation and
    // the (non-FIXED) hint search below.
    let raw_end = addr.checked_add(length).ok_or(AxError::InvalidInput)?;
    let end = raw_end.align_up(page_size);
    if end < raw_end {
        return Err(AxError::InvalidInput);
    }
    let mut length = end - aligned;

    let file = if anonymous {
        None
    } else {
        Some(get_file_like(fd)?)
    };
    // Only probe `device_mmap` for MAP_SHARED. MAP_PRIVATE always maps
    // through the file_mmap/CoW path below and never consumes this result, so
    // calling it would be wasted work — and for fds whose `device_mmap` has
    // side effects (e.g. a perf-event ringbuf allocation) it would leave the
    // fd in a half-initialized state that rejects the later real MAP_SHARED
    // mapping. Probe lazily here, then commit it in the MAP_SHARED arm.
    let mut device_mmap_top = if matches!(map_type, MmapFlags::SHARED) {
        file.as_ref()
            .map(|fl| fl.device_mmap(offset as u64, length as u64))
    } else {
        None
    };

    // Validate file_mmap permissions and memfd seals before any destructive
    // MAP_FIXED unmap (Linux `do_mmap` ordering; avoids tearing down the old
    // mapping on `EACCES` / `EPERM`). MAP_PRIVATE always uses file_mmap below,
    // even if device_mmap reports a direct mapping.
    if let Some(ref fl) = file {
        let needs_file_mmap_checks = match map_type {
            MmapFlags::PRIVATE => true,
            MmapFlags::SHARED => {
                // Ok(None) and Err(_) both mean "fall back to file_mmap"
                // (memfd, regular files). Direct device mappings do not.
                match device_mmap_top
                    .as_ref()
                    .expect("file-backed mmap has cached device_mmap")
                {
                    #[cfg(feature = "rknpu")]
                    Ok(DeviceMmap::PhysicalCached(..)) => false,
                    Ok(DeviceMmap::Physical(..))
                    | Ok(DeviceMmap::PhysicalResolved(..))
                    | Ok(DeviceMmap::PhysicalPages(..))
                    | Ok(DeviceMmap::Cache(_)) => false,
                    Ok(DeviceMmap::None) | Err(_) => true,
                }
            }
            _ => false,
        };
        if needs_file_mmap_checks {
            let (_backend, flags) = fl.file_mmap()?;
            if !flags.contains(FileFlags::READ) {
                return Err(AxError::PermissionDenied);
            }
            if matches!(map_type, MmapFlags::SHARED) && permission_flags.contains(MmapProt::WRITE) {
                if !flags.contains(FileFlags::WRITE) {
                    return Err(AxError::PermissionDenied);
                }
                // Linux: F_SEAL_WRITE forbids shared writable mappings, but still allows
                // MAP_PRIVATE|PROT_WRITE because it does not modify the underlying file.
                memfd_check_write_seal(fl)?;
            }
        }
    }

    let start = if map_flags.intersects(MmapFlags::FIXED | MmapFlags::FIXED_NOREPLACE) {
        let dst_addr = VirtAddr::from(aligned);
        if !map_flags.contains(MmapFlags::FIXED_NOREPLACE) {
            aspace.unmap(dst_addr, length)?;
        }
        dst_addr
    } else {
        let align = page_size as usize;
        // Defense-in-depth (#242): cap the search upper bound to
        // `USER_STACK_TOP - STACK_GUARD_GAP` so a non-FIXED mmap (e.g. V8's
        // 4 GiB PROT_NONE pointer-compression cage reservation) can never
        // land in the slot immediately above the user stack. Linux uses an
        // analogous `stack_guard_gap` (default 256 pages) in
        // `mm/mmap.c::vma_compute_gap`. Explicit MAP_FIXED requests are
        // unaffected.
        const STACK_GUARD_GAP: usize = 0x10_0000; // 1 MiB
        let upper = crate::config::USER_STACK_TOP.saturating_sub(STACK_GUARD_GAP);
        let limit = VirtAddrRange::new(aspace.base(), VirtAddr::from(upper));
        aspace
            .find_free_area(VirtAddr::from(aligned), length, limit, align)
            .or(aspace.find_free_area(aspace.base(), length, limit, align))
            .ok_or(AxError::NoMemory)?
    };

    // IonBufferFile 特殊处理：直接线性映射物理地址，跳过通用 file_mmap/device_mmap 路径。
    // 这样可以避免通用路径中 `range.start += offset` 对 Ion buffer 的错误偏移。
    #[cfg(feature = "sg2002")]
    if let Some(ref file) = file {
        use crate::file::ion::IonBufferFile;
        if let Some(ion_file) = file.downcast_ref::<IonBufferFile>() {
            let range = ion_file.phys_range();
            let buffer_len = range.size().align_up(page_size);
            let map_length = length.align_up(page_size);
            info!(
                "Ion buffer mmap: phys_addr=0x{:x}, buffer_size={}, requested_length={}, \
                 map_length={}",
                range.start.as_usize(),
                range.size(),
                length,
                map_length
            );
            if map_length == 0 {
                warn!("Ion buffer mmap: map_length is 0, this should not happen");
                return Err(AxError::InvalidInput);
            }
            // 不允许越过 buffer 物理边界：否则 Backend::new_linear 会按线性偏移把
            // `range.start + range.size()` 之后的物理页映射进进程地址空间。
            if map_length > buffer_len {
                warn!(
                    "Ion buffer mmap: requested length {} exceeds buffer size {}",
                    map_length, buffer_len
                );
                return Err(AxError::InvalidInput);
            }
            let mut ion_mapping_flags: MappingFlags = permission_flags.into();
            ion_mapping_flags |= MappingFlags::UNCACHED;
            let backend = Backend::new_linear_anchored(
                start,
                start.as_usize() as isize - range.start.as_usize() as isize,
                true,
                ion_file.buffer().clone(),
            );
            let populate = map_flags.contains(MmapFlags::POPULATE);
            aspace.map(start, map_length, ion_mapping_flags, populate, backend)?;
            drop(aspace);
            info!(
                "Ion buffer mmap success: vaddr=0x{:x}, length={}",
                start.as_usize(),
                map_length
            );
            return Ok(start.as_usize() as _);
        }
    }

    let mut mapping_flags: MappingFlags = permission_flags.into();

    let backend = match map_type {
        MmapFlags::SHARED => {
            if let Some(ref file) = file {
                match device_mmap_top
                    .take()
                    .expect("file-backed mmap has cached device_mmap")
                {
                    Ok(DeviceMmap::Physical(mut range, retain)) => {
                        mapping_flags |= MappingFlags::UNCACHED;
                        range.start += offset;
                        if range.is_empty() {
                            return Err(AxError::InvalidInput);
                        }
                        length = length.min(range.size().align_down(page_size));
                        let pa_va_offset =
                            start.as_usize() as isize - range.start.as_usize() as isize;
                        match retain {
                            Some(retain) => {
                                Backend::new_linear_anchored(start, pa_va_offset, true, retain)
                            }
                            None => Backend::new_linear(start, pa_va_offset, true),
                        }
                    }
                    #[cfg(feature = "rknpu")]
                    Ok(DeviceMmap::PhysicalCached(mut range, retain)) => {
                        range.start += offset;
                        if range.is_empty() {
                            return Err(AxError::InvalidInput);
                        }
                        length = length.min(range.size().align_down(page_size));
                        let pa_va_offset =
                            start.as_usize() as isize - range.start.as_usize() as isize;
                        match retain {
                            Some(retain) => {
                                Backend::new_linear_anchored(start, pa_va_offset, true, retain)
                            }
                            None => Backend::new_linear(start, pa_va_offset, true),
                        }
                    }
                    Ok(DeviceMmap::PhysicalResolved(range, retain)) => {
                        mapping_flags |= MappingFlags::UNCACHED;
                        if range.is_empty() {
                            return Err(AxError::InvalidInput);
                        }
                        length = length.min(range.size().align_down(page_size));
                        let pa_va_offset =
                            start.as_usize() as isize - range.start.as_usize() as isize;
                        match retain {
                            Some(retain) => {
                                Backend::new_linear_anchored(start, pa_va_offset, true, retain)
                            }
                            None => Backend::new_linear(start, pa_va_offset, true),
                        }
                    }
                    Ok(DeviceMmap::PhysicalPages(pages, retain)) => {
                        length = length.min(pages.len() * PAGE_SIZE_4K);
                        Backend::new_shared(
                            start,
                            Arc::new(SharedPages::borrowed(pages, PageSize::Size4K, retain)?),
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
                                    &curr.as_thread().proc_data.aspace(),
                                    true,
                                )
                            }
                            FileBackend::Direct(loc) => {
                                let device = loc
                                    .entry()
                                    .downcast::<Device>()
                                    .map_err(|_| AxError::NoSuchDevice)?;

                                match device.mmap(offset as u64, length as u64) {
                                    DeviceMmap::None => {
                                        return Err(AxError::NoSuchDevice);
                                    }
                                    DeviceMmap::Physical(range, retain) => {
                                        mapping_flags |= MappingFlags::UNCACHED;
                                        if range.is_empty() {
                                            return Err(AxError::InvalidInput);
                                        }
                                        length =
                                            capped_device_map_len(length, range.size(), page_size);
                                        let pa_va_offset = start.as_usize() as isize
                                            - range.start.as_usize() as isize;
                                        match retain {
                                            Some(retain) => Backend::new_linear_anchored(
                                                start,
                                                pa_va_offset,
                                                true,
                                                retain,
                                            ),
                                            None => Backend::new_linear(start, pa_va_offset, true),
                                        }
                                    }
                                    #[cfg(feature = "rknpu")]
                                    DeviceMmap::PhysicalCached(range, retain) => {
                                        if range.is_empty() {
                                            return Err(AxError::InvalidInput);
                                        }
                                        length =
                                            capped_device_map_len(length, range.size(), page_size);
                                        let pa_va_offset = start.as_usize() as isize
                                            - range.start.as_usize() as isize;
                                        match retain {
                                            Some(retain) => Backend::new_linear_anchored(
                                                start,
                                                pa_va_offset,
                                                true,
                                                retain,
                                            ),
                                            None => Backend::new_linear(start, pa_va_offset, true),
                                        }
                                    }
                                    DeviceMmap::PhysicalResolved(range, retain) => {
                                        mapping_flags |= MappingFlags::UNCACHED;
                                        if range.is_empty() {
                                            return Err(AxError::InvalidInput);
                                        }
                                        length =
                                            capped_device_map_len(length, range.size(), page_size);
                                        let pa_va_offset = start.as_usize() as isize
                                            - range.start.as_usize() as isize;
                                        match retain {
                                            Some(retain) => Backend::new_linear_anchored(
                                                start,
                                                pa_va_offset,
                                                true,
                                                retain,
                                            ),
                                            None => Backend::new_linear(start, pa_va_offset, true),
                                        }
                                    }
                                    DeviceMmap::PhysicalPages(pages, retain) => {
                                        length = length.min(pages.len() * PAGE_SIZE_4K);
                                        Backend::new_shared(
                                            start,
                                            Arc::new(SharedPages::borrowed(
                                                pages,
                                                PageSize::Size4K,
                                                retain,
                                            )?),
                                        )
                                    }
                                    DeviceMmap::Cache(cache) => Backend::new_file(
                                        start,
                                        cache,
                                        flags,
                                        offset,
                                        &curr.as_thread().proc_data.aspace(),
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
    aspace.map(start, length, mapping_flags, populate, backend)?;
    drop(aspace);

    // perf side-band: an executable, file-backed mapping is (almost always) a
    // shared library the dynamic loader just mapped. Emit a PERF_RECORD_MMAP2 to
    // any per-task perf event monitoring this task so `perf report` can symbolize
    // its samples. The perf ring itself is mapped PROT_READ|WRITE (no EXEC), so it
    // is naturally excluded; anonymous executable maps (no file) too.
    #[cfg(target_arch = "aarch64")]
    if permission_flags.contains(MmapProt::EXEC)
        && let Some(ref file) = file
    {
        let mut prot = 0u32;
        if permission_flags.contains(MmapProt::READ) {
            prot |= 1;
        }
        if permission_flags.contains(MmapProt::WRITE) {
            prot |= 2;
        }
        prot |= 4; // PROT_EXEC
        let path = file.path();
        crate::perf::task::on_mmap_sideband(
            curr.as_thread(),
            start.as_usize(),
            length,
            offset,
            prot,
            matches!(map_type, MmapFlags::SHARED),
            &path,
        );
    }

    Ok(start.as_usize() as _)
}

pub fn sys_munmap(addr: usize, length: usize) -> AxResult<isize> {
    // man 2 munmap: "length was 0" → EINVAL (since Linux 2.6.12).
    if length == 0 {
        return Err(AxError::InvalidInput);
    }
    debug!("sys_munmap <= addr: {addr:#x}, length: {length:x}");
    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.aspace();
    let mut aspace = aspace_arc.lock();
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
    let aspace_arc = curr.as_thread().proc_data.aspace();
    let mut aspace = aspace_arc.lock();
    let length = align_up_4k(length);
    let start_addr = VirtAddr::from(addr);
    // man 2 mprotect: if any page in [addr, addr+len) lacks a mapping → ENOMEM.
    // Linux validates the whole range, not just the first page: an unmapped hole
    // in the middle of `[mapped][hole][mapped]` must fail instead of silently
    // protecting only the mapped fragments. `can_access_range` with an empty
    // access mask is a pure contiguous-coverage check (every area's flags
    // trivially contain the empty set), so it returns false on the first gap.
    // We pre-check and leave the mapping untouched on failure, i.e. report
    // ENOMEM atomically without any half-applied protection — the errno real
    // programs test for.
    if !aspace.can_access_range(start_addr, length, MappingFlags::empty()) {
        return Err(AxError::NoMemory);
    }
    if permission_flags.contains(MmapProt::WRITE) {
        for (_frag_start, _frag_size, _old_flags, backend) in
            aspace.areas_in_range(start_addr, length)
        {
            memfd_check_write_seal_for_shared_file_backend(&backend)?;
        }
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
    let aspace_ref = &curr.as_thread().proc_data.aspace();
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

    // man 2 madvise: addr must be page-aligned.
    if !addr.is_multiple_of(PageSize::Size4K as usize) {
        return Err(AxError::InvalidInput);
    }

    if length == 0 {
        return Ok(0);
    }

    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.aspace();
    let mut aspace = aspace_arc.lock();

    // man 2 madvise ENOMEM: the WHOLE page-aligned range must be mapped, not just
    // the first page. Checking only `find_area(addr)` lets a `[mapped][hole][mapped]`
    // range succeed while `discard_range` silently skips the hole; Linux returns
    // ENOMEM. An empty access mask makes `can_access_range` a pure contiguous-
    // coverage check (every area's flags trivially contain the empty set).
    if !aspace.can_access_range(
        VirtAddr::from(addr),
        align_up_4k(length),
        MappingFlags::empty(),
    ) {
        return Err(AxError::NoMemory);
    }

    // MADV_DONTNEED: drop the pages now; next access re-faults to a fresh zero
    // page (anon) / re-read (file CoW). MADV_FREE is lazy in Linux but a
    // synchronous drop is a correct, conservative implementation (per man 2
    // madvise the app must not rely on stale contents after MADV_FREE).
    // Go's runtime relies on this to return idle heap spans — without it the
    // committed working set grows until OOM. DONTNEED_LOCKED behaves like
    // DONTNEED here (we do not honor mlock).
    match advice as u32 {
        MADV_DONTNEED | MADV_FREE | MADV_DONTNEED_LOCKED => {
            let length = align_up_4k(length);
            aspace.discard_range(VirtAddr::from(addr), length)?;
        }
        _ => {}
    }

    Ok(0)
}

pub fn sys_msync(addr: usize, length: usize, flags: u32) -> AxResult<isize> {
    debug!("sys_msync <= addr: {addr:#x}, length: {length:x}, flags: {flags:#x}");

    if !addr.is_multiple_of(PAGE_SIZE_4K) {
        return Err(AxError::InvalidInput);
    }

    let valid_flags = MS_SYNC | MS_ASYNC | MS_INVALIDATE;
    if flags & !valid_flags != 0 {
        return Err(AxError::InvalidInput);
    }
    if flags & MS_SYNC != 0 && flags & MS_ASYNC != 0 {
        return Err(AxError::InvalidInput);
    }

    if length == 0 {
        return Ok(0);
    }

    let start = VirtAddr::from(addr);
    let end_val = addr.checked_add(length).ok_or(AxError::InvalidInput)?;
    let end = VirtAddr::from(end_val);

    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.aspace();
    let writebacks: Vec<_> = {
        let aspace = aspace_arc.lock();
        let mut writebacks = Vec::new();
        let mut cursor = start;
        while cursor < end {
            let area = match aspace.find_area(cursor) {
                Some(a) => a,
                None => return Err(AxError::NoMemory),
            };
            let range_start = area.start().max(start);
            let range_end = area.end().min(end);
            if let Backend::File(file_backend) = area.backend()
                && file_backend.is_shared()
            {
                writebacks.push((file_backend.clone(), range_start, range_end));
            }
            cursor = area.end();
        }
        writebacks
    };

    for (file_backend, range_start, range_end) in writebacks {
        file_backend.writeback_range(range_start, range_end)?;
    }

    Ok(0)
}

pub fn sys_mlock(addr: usize, length: usize) -> AxResult<isize> {
    sys_mlock2(addr, length, 0)
}

pub fn sys_mlock2(addr: usize, length: usize, flags: u32) -> AxResult<isize> {
    // Linux `mlock2` accepts only `flags == 0` or `MLOCK_ONFAULT`; any other bit
    // is rejected with EINVAL and must produce no populate/fault side effect.
    const MLOCK_ONFAULT: u32 = 0x01;
    if flags & !MLOCK_ONFAULT != 0 {
        return Err(AxError::InvalidInput);
    }
    if length == 0 {
        return Ok(0);
    }
    let aligned = addr.align_down(PageSize::Size4K);
    // `checked_add` guards `addr + length`, but `align_up` itself adds
    // `PAGE_SIZE - 1` internally and can still wrap a near-`usize::MAX` end to a
    // small value; detect that wrap (end < raw_end) and reject, as Linux rejects
    // an out-of-range mlock with EINVAL rather than locking a tiny wrapped range.
    let raw_end = addr.checked_add(length).ok_or(AxError::InvalidInput)?;
    let end = raw_end.align_up(PageSize::Size4K);
    if end < raw_end {
        return Err(AxError::InvalidInput);
    }
    let size = end - aligned;

    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.aspace();
    let mut aspace = aspace_arc.lock();
    let start = VirtAddr::from(aligned);
    if flags & MLOCK_ONFAULT != 0 {
        // MLOCK_ONFAULT: lock the range but bring pages in lazily (no eager
        // fault). Still report ENOMEM if the range has an unmapped hole, as
        // Linux does. An empty access mask makes `can_access_range` a pure
        // contiguous-coverage check.
        if !aspace.can_access_range(start, size, MappingFlags::empty()) {
            return Err(AxError::NoMemory);
        }
    } else {
        // Plain mlock (flags == 0): honor the "fault now" contract by faulting
        // the whole range in, reporting ENOMEM on any unmapped page. On this
        // no-swap kernel the faulted pages then stay resident, satisfying mlock's
        // residency guarantee. `populate_area` is the MAP_POPULATE primitive.
        aspace.populate_area(start, size, MappingFlags::READ)?;
    }
    Ok(0)
}
