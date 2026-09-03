use alloc::{sync::Arc, vec::Vec};

use ax_fs_ng::vfs::{FileBackend, FileFlags};
use ax_memory_addr::{
    MemoryAddr, PAGE_SIZE_2M, PAGE_SIZE_4K, VirtAddr, VirtAddrRange,
};
use ax_memory_set::MappingError;
use ax_runtime::hal::paging::MappingFlags;
use ax_task::current;
use linux_raw_sys::general::*;

use crate::{
    StarryError, StarryResult,
    file::get_file_like,
    mm::{
        HugePageAdvice, MappingOperation, MappingPublication, MemlockLimit, SharedMemoryObject,
        VmaAccessPattern, VmaAdviceUpdate, VmaLockMode, VmaMremapSource,
    },
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

fn reported_mapping_flags_from_prot(value: MmapProt) -> MappingFlags {
    let mut flags = MappingFlags::empty();
    if value.contains(MmapProt::READ) {
        flags |= MappingFlags::READ;
    }
    if value.contains(MmapProt::WRITE) {
        flags |= MappingFlags::WRITE;
    }
    if value.contains(MmapProt::EXEC) {
        flags |= MappingFlags::EXECUTE;
    }
    if !flags.is_empty() {
        flags |= MappingFlags::USER;
    }
    flags
}

/// Derive the immutable permission envelope from the mapping source rather
/// than from its initial `PROT_*` value.
///
/// In particular, musl reserves a thread stack as `PROT_NONE` and then makes
/// the non-guard portion readable and writable with `mprotect`.  Treating the
/// initial empty PTE flags as `VM_MAY* == 0` makes that valid transition fail
/// with `EACCES`.  Anonymous and private COW mappings may acquire any ordinary
/// user permission later; shared file mappings may acquire write permission
/// only when the file was opened writable.  Linear device mappings keep their
/// driver-selected envelope unchanged.
fn maximum_mapping_flags_for_backend(
    backend: &MappingOperation,
    current: MappingFlags,
) -> MappingFlags {
    backend.maximum_mapping_flags(current)
}

fn checked_align_up(value: usize, alignment: usize) -> StarryResult<usize> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(StarryError::InvalidInput);
    }
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded & !(alignment - 1))
        .ok_or(StarryError::InvalidInput)
}

fn capped_device_map_len(
    request_len: usize,
    available_len: usize,
    page_size: usize,
) -> StarryResult<usize> {
    Ok(request_len.min(checked_align_up(available_len, page_size)?))
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
        /// Lock the mapping and populate it eagerly.
        const LOCKED = MAP_LOCKED;
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
) -> StarryResult<isize> {
    if length == 0 {
        return Err(StarryError::InvalidInput);
    }

    let curr = current();
    let curr_aspace = curr.as_thread().proc_data.pin_aspace()?;
    let Some(permission_flags) = MmapProt::from_bits(prot) else {
        return Err(StarryError::InvalidInput);
    };
    let map_flags = match MmapFlags::from_bits(flags) {
        Some(flags) => flags,
        None => {
            warn!("unknown mmap flags: {flags}");
            if (flags & MmapFlags::TYPE.bits()) == MmapFlags::SHARED_VALIDATE.bits() {
                return Err(StarryError::OperationNotSupported);
            }
            MmapFlags::from_bits_truncate(flags)
        }
    };
    if map_flags.contains(MmapFlags::SYNC) {
        return Err(StarryError::OperationNotSupported);
    }
    let mapping_memlock_limit = if map_flags.contains(MmapFlags::LOCKED) {
        let byte_limit = curr.as_thread().proc_data.rlim.read()[RLIMIT_MEMLOCK].current;
        let limit = MemlockLimit::for_mapping(
            byte_limit,
            curr.as_thread().cred().has_cap_ipc_lock(),
        );
        if !limit.can_lock() {
            return Err(StarryError::OperationNotPermitted);
        }
        Some(limit)
    } else {
        None
    };
    let mut aspace = curr_aspace.lock();
    let anonymous = map_flags.contains(MmapFlags::ANONYMOUS);
    let map_type = match flags & MmapFlags::TYPE.bits() {
        MAP_SHARED => MmapFlags::SHARED,
        MAP_SHARED_VALIDATE if !anonymous => MmapFlags::SHARED,
        MAP_PRIVATE => MmapFlags::PRIVATE,
        _ => return Err(StarryError::InvalidInput),
    };
    if map_flags.contains(MmapFlags::HUGE_1GB) {
        // Starry's typed split/deposit contract is currently PMD-sized. Do not
        // admit a 1 GiB mapping that later cannot satisfy Linux partial
        // munmap/mprotect/mremap semantics.
        return Err(StarryError::OperationNotSupported);
    }
    if map_flags.contains(MmapFlags::HUGE)
        && (!anonymous || map_type != MmapFlags::PRIVATE)
    {
        // Shared/file huge mappings need hugetlbfs/shmem ownership that is not
        // implemented yet. Explicit failure is safer than silently creating a
        // 4 KiB mapping with a huge-page ABI request.
        return Err(StarryError::OperationNotSupported);
    }
    let offset: usize = offset.try_into().map_err(|_| StarryError::InvalidInput)?;
    if !offset.is_multiple_of(PAGE_SIZE_4K) {
        return Err(StarryError::InvalidInput);
    }
    if !anonymous && fd < 0 {
        return Err(StarryError::BadFileDescriptor);
    }

    debug!(
        "sys_mmap <= addr: {addr:#x?}, length: {length:#x?}, prot: {permission_flags:?}, flags: \
         {map_flags:?}, fd: {fd:?}, offset: {offset:?}"
    );

    let page_size = if map_flags.contains(MmapFlags::HUGE) {
        PAGE_SIZE_2M
    } else {
        PAGE_SIZE_4K
    };

    let fixed = map_flags.intersects(MmapFlags::FIXED | MmapFlags::FIXED_NOREPLACE);
    if fixed && !addr.is_multiple_of(page_size) {
        return Err(StarryError::InvalidInput);
    }

    // Linux rounds the hint and the requested length independently. The low
    // bits of a non-fixed hint affect only placement; folding them into the
    // length would turn `mmap(hint + 1, PAGE_SIZE, ...)` into a two-page map.
    let length = checked_align_up(length, page_size)?;
    let aligned = addr.align_down(page_size);
    if fixed {
        addr.checked_add(length)
            .ok_or(StarryError::InvalidInput)?;
    }
    let mut length = length;

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
    let device_mmap_top = if matches!(map_type, MmapFlags::SHARED) {
        file.as_ref()
            .map(|fl| fl.device_mmap(offset as u64, length as u64))
    } else {
        None
    };
    // A device implementation has committed to this mapping contract once it
    // returns an error. Reject it before MAP_FIXED can tear down an existing
    // mapping; only `DeviceMmap::None` selects the file-backed fallback.
    let mut device_mmap_top = match device_mmap_top {
        Some(Err(error)) => return Err(error),
        result => result,
    };

    // Validate file_mmap permissions and memfd seals before any destructive
    // MAP_FIXED unmap (Linux `do_mmap` ordering; avoids tearing down the old
    // mapping on `EACCES` / `EPERM`). MAP_PRIVATE always uses file_mmap below,
    // even if device_mmap reports a direct mapping.
    if let Some(ref fl) = file {
        let needs_file_mmap_checks = match map_type {
            MmapFlags::PRIVATE => true,
            MmapFlags::SHARED => {
                // `DeviceMmap::None` means "fall back to file_mmap" (memfd,
                // regular files). A device implementation's error is already
                // committed and must survive to userspace.
                let Some(device) = device_mmap_top.as_ref() else {
                    return Err(StarryError::BadState);
                };
                match device {
                    Ok(DeviceMmap::PhysicalCached(..)) => false,
                    Ok(DeviceMmap::Physical(..))
                    | Ok(DeviceMmap::PhysicalResolved(..))
                    | Ok(DeviceMmap::PhysicalPages(..))
                    | Ok(DeviceMmap::Cache(_)) => false,
                    Ok(DeviceMmap::None) => true,
                    Err(_) => false,
                }
            }
            _ => false,
        };
        if needs_file_mmap_checks {
            let (_backend, flags) = fl.file_mmap()?;
            if !flags.contains(FileFlags::READ) {
                return Err(StarryError::PermissionDenied);
            }
            if matches!(map_type, MmapFlags::SHARED) && permission_flags.contains(MmapProt::WRITE) {
                if !flags.contains(FileFlags::WRITE) {
                    return Err(StarryError::PermissionDenied);
                }
                // Linux: F_SEAL_WRITE forbids shared writable mappings, but still allows
                // MAP_PRIVATE|PROT_WRITE because it does not modify the underlying file.
                memfd_check_write_seal(fl)?;
            }
        }
    }

    let start = if map_flags.intersects(MmapFlags::FIXED | MmapFlags::FIXED_NOREPLACE) {
        let dst_addr = VirtAddr::from(aligned);
        // Keep the old mapping until the replacement backend has passed all
        // validation. `AddrSpace::map_with_permissions_replace` performs the
        // preimage-aware MAP_FIXED operation; tearing it down here would turn
        // a later allocation/permission error into a destructive partial
        // syscall.
        dst_addr
    } else {
        let align = page_size;
        // Defense-in-depth (#242): cap the search upper bound to
        // the current MM's `stack_top - STACK_GUARD_GAP` so a non-FIXED mmap
        // (e.g. V8's 4 GiB PROT_NONE pointer-compression cage reservation) can
        // never land in the slot immediately above the user stack. Linux uses
        // an analogous `stack_guard_gap` (default 256 pages) in
        // `mm/mmap.c::vma_compute_gap`. Explicit MAP_FIXED requests are unaffected.
        const STACK_GUARD_GAP: usize = 0x10_0000; // 1 MiB
        let upper = aspace
            .stack_top()
            .as_usize()
            .saturating_sub(STACK_GUARD_GAP);
        let limit = VirtAddrRange::new(aspace.base(), VirtAddr::from(upper));
        aspace
            .find_free_area(VirtAddr::from(aligned), length, limit, align)
            .or(aspace.find_free_area(aspace.base(), length, limit, align))
            .ok_or(StarryError::NoMemory)?
    };

    // IonBufferFile 特殊处理：直接线性映射物理地址，跳过通用 file_mmap/device_mmap 路径。
    // 这样可以避免通用路径中 `range.start += offset` 对 Ion buffer 的错误偏移。
    #[cfg(feature = "sg2002")]
    if let Some(ref file) = file {
        use crate::file::ion::IonBufferFile;
        if let Some(ion_file) = file.downcast_ref::<IonBufferFile>() {
            let range = ion_file.phys_range();
            let buffer_len = checked_align_up(range.size(), page_size)?;
            let map_length = checked_align_up(length, page_size)?;
            let reported_mapping_flags = reported_mapping_flags_from_prot(permission_flags);
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
                return Err(StarryError::InvalidInput);
            }
            // 不允许越过 buffer 物理边界：否则 MappingOperation::new_linear 会按线性偏移把
            // `range.start + range.size()` 之后的物理页映射进进程地址空间。
            if map_length > buffer_len {
                warn!(
                    "Ion buffer mmap: requested length {} exceeds buffer size {}",
                    map_length, buffer_len
                );
                return Err(StarryError::InvalidInput);
            }
            let mut ion_mapping_flags: MappingFlags = permission_flags.into();
            ion_mapping_flags |= MappingFlags::UNCACHED;
            let backend = MappingOperation::new_linear_anchored(
                start,
                range.start,
                true,
                ion_file.buffer().clone(),
            );
            let lock_mode = if map_flags.contains(MmapFlags::LOCKED) {
                VmaLockMode::Locked
            } else {
                VmaLockMode::Unlocked
            };
            let populate = map_flags.intersects(MmapFlags::POPULATE | MmapFlags::LOCKED);
            let replace_existing = map_flags.contains(MmapFlags::FIXED)
                && !map_flags.contains(MmapFlags::FIXED_NOREPLACE);
            aspace.map_with_permissions_publication(
                start,
                map_length,
                crate::mm::MappingPermissions {
                    current: ion_mapping_flags,
                    reported: reported_mapping_flags,
                    maximum: ion_mapping_flags,
                },
                populate,
                backend,
                MappingPublication::mmap(
                    replace_existing,
                    lock_mode,
                    mapping_memlock_limit,
                ),
            )?;
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
    let reported_mapping_flags = reported_mapping_flags_from_prot(permission_flags);

    let backend = match map_type {
        MmapFlags::SHARED => {
            if let Some(ref file) = file {
                let Some(device_mmap) = device_mmap_top.take() else {
                    return Err(StarryError::BadState);
                };
                match device_mmap {
                    Ok(DeviceMmap::Physical(mut range, retain)) => {
                        mapping_flags |= MappingFlags::UNCACHED;
                        range.start = range
                            .start
                            .checked_add(offset)
                            .ok_or(StarryError::InvalidInput)?;
                        if range.is_empty() {
                            return Err(StarryError::InvalidInput);
                        }
                        length = length.min(range.size().align_down(page_size));
                        match retain {
                            Some(retain) => {
                                MappingOperation::new_linear_anchored(start, range.start, true, retain)
                            }
                            None => MappingOperation::new_linear(start, range.start, true),
                        }
                    }
                    Ok(DeviceMmap::PhysicalCached(mut range, retain)) => {
                        range.start = range
                            .start
                            .checked_add(offset)
                            .ok_or(StarryError::InvalidInput)?;
                        if range.is_empty() {
                            return Err(StarryError::InvalidInput);
                        }
                        length = length.min(range.size().align_down(page_size));
                        match retain {
                            Some(retain) => {
                                MappingOperation::new_linear_anchored(start, range.start, true, retain)
                            }
                            None => MappingOperation::new_linear(start, range.start, true),
                        }
                    }
                    Ok(DeviceMmap::PhysicalResolved(range, retain)) => {
                        mapping_flags |= MappingFlags::UNCACHED;
                        if range.is_empty() {
                            return Err(StarryError::InvalidInput);
                        }
                        length = length.min(range.size().align_down(page_size));
                        match retain {
                            Some(retain) => {
                                MappingOperation::new_linear_anchored(start, range.start, true, retain)
                            }
                            None => MappingOperation::new_linear(start, range.start, true),
                        }
                    }
                    Ok(DeviceMmap::PhysicalPages(pages, retain)) => {
                        length = length.min(pages.len() * PAGE_SIZE_4K);
                        MappingOperation::new_shared(
                            start,
                            Arc::new(SharedMemoryObject::borrowed(
                                pages,
                                PAGE_SIZE_4K,
                                retain,
                            )?),
                        )
                    }
                    Ok(DeviceMmap::None) => {
                        let (backend, flags) = file.file_mmap()?;
                        // man 2 mmap EACCES: a file mapping requires the fd to be
                        // open for reading, and MAP_SHARED+PROT_WRITE additionally
                        // requires the fd to be open for writing.
                        if !flags.contains(FileFlags::READ) {
                            return Err(StarryError::PermissionDenied);
                        }
                        if permission_flags.contains(MmapProt::WRITE)
                            && !flags.contains(FileFlags::WRITE)
                        {
                            return Err(StarryError::PermissionDenied);
                        }
                        match backend.clone() {
                            FileBackend::Cached(cache) => {
                                // TODO(mivik): file mmap page size
                                MappingOperation::new_file(
                                    start,
                                    cache,
                                    flags,
                                    offset,
                                    true,
                                )?
                            }
                            FileBackend::Direct(loc) => {
                                let device = loc
                                    .entry()
                                    .downcast::<Device>()
                                    .map_err(|_| StarryError::NoSuchDevice)?;

                                match device.mmap(offset as u64, length as u64) {
                                    DeviceMmap::None => {
                                        return Err(StarryError::NoSuchDevice);
                                    }
                                    DeviceMmap::Physical(range, retain) => {
                                        mapping_flags |= MappingFlags::UNCACHED;
                                        if range.is_empty() {
                                            return Err(StarryError::InvalidInput);
                                        }
                                        length = capped_device_map_len(
                                            length,
                                            range.size(),
                                            page_size,
                                        )?;
                                        match retain {
                                            Some(retain) => MappingOperation::new_linear_anchored(
                                                start,
                                                range.start,
                                                true,
                                                retain,
                                            ),
                                            None => MappingOperation::new_linear(start, range.start, true),
                                        }
                                    }
                                    DeviceMmap::PhysicalCached(range, retain) => {
                                        if range.is_empty() {
                                            return Err(StarryError::InvalidInput);
                                        }
                                        length = capped_device_map_len(
                                            length,
                                            range.size(),
                                            page_size,
                                        )?;
                                        match retain {
                                            Some(retain) => MappingOperation::new_linear_anchored(
                                                start,
                                                range.start,
                                                true,
                                                retain,
                                            ),
                                            None => MappingOperation::new_linear(start, range.start, true),
                                        }
                                    }
                                    DeviceMmap::PhysicalResolved(range, retain) => {
                                        mapping_flags |= MappingFlags::UNCACHED;
                                        if range.is_empty() {
                                            return Err(StarryError::InvalidInput);
                                        }
                                        length = capped_device_map_len(
                                            length,
                                            range.size(),
                                            page_size,
                                        )?;
                                        match retain {
                                            Some(retain) => MappingOperation::new_linear_anchored(
                                                start,
                                                range.start,
                                                true,
                                                retain,
                                            ),
                                            None => MappingOperation::new_linear(start, range.start, true),
                                        }
                                    }
                                    DeviceMmap::PhysicalPages(pages, retain) => {
                                        length = length.min(pages.len() * PAGE_SIZE_4K);
                                        MappingOperation::new_shared(
                                            start,
                                            Arc::new(SharedMemoryObject::borrowed(
                                                pages,
                                                PAGE_SIZE_4K,
                                                retain,
                                            )?),
                                        )
                                    }
                                    DeviceMmap::Cache(cache) => MappingOperation::new_file(
                                        start,
                                        cache,
                                        flags,
                                        offset,
                                        true,
                                    )?,
                                }
                            }
                        }
                    }
                    Ok(_) => return Err(StarryError::InvalidInput),
                    Err(error) => return Err(error),
                }
            } else {
                MappingOperation::new_shared(
                    start,
                    Arc::new(SharedMemoryObject::allocate(length, PAGE_SIZE_4K)?),
                )
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
                    return Err(StarryError::PermissionDenied);
                }
                MappingOperation::new_cow(start, page_size, backend, offset as u64, None, false)
            } else {
                MappingOperation::new_alloc(start, page_size, "")
            }
        }
        _ => return Err(StarryError::InvalidInput),
    };

    let lock_mode = if map_flags.contains(MmapFlags::LOCKED) {
        VmaLockMode::Locked
    } else {
        VmaLockMode::Unlocked
    };
    let populate = map_flags.intersects(MmapFlags::POPULATE | MmapFlags::LOCKED);
    let replace_existing = map_flags.contains(MmapFlags::FIXED)
        && !map_flags.contains(MmapFlags::FIXED_NOREPLACE);
    let maximum_mapping_flags = maximum_mapping_flags_for_backend(&backend, mapping_flags);
    aspace.map_with_permissions_publication(
        start,
        length,
        crate::mm::MappingPermissions {
            current: mapping_flags,
            reported: reported_mapping_flags,
            maximum: maximum_mapping_flags,
        },
        populate,
        backend,
        MappingPublication::mmap(replace_existing, lock_mode, mapping_memlock_limit),
    )?;
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

pub fn sys_munmap(addr: usize, length: usize) -> StarryResult<isize> {
    // man 2 munmap: "length was 0" → EINVAL (since Linux 2.6.12).
    if length == 0 {
        return Err(StarryError::InvalidInput);
    }
    // The kernel must never silently round an unaligned starting address:
    // Linux requires `addr` itself to be page aligned and only rounds the
    // length.  Checking before acquiring the address-space lock also keeps a
    // rejected request side-effect free.
    if !addr.is_multiple_of(PAGE_SIZE_4K) {
        return Err(StarryError::InvalidInput);
    }
    debug!("sys_munmap <= addr: {addr:#x}, length: {length:x}");
    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.pin_aspace()?;
    let mut aspace = aspace_arc.lock();
    let length = checked_align_up(length, PAGE_SIZE_4K)?;
    let start_addr = VirtAddr::from(addr);
    aspace.unmap(start_addr, length)?;
    Ok(0)
}

pub fn sys_mprotect(addr: usize, length: usize, prot: u32) -> StarryResult<isize> {
    // TODO: implement PROT_GROWSUP & PROT_GROWSDOWN
    let Some(permission_flags) = MmapProt::from_bits(prot) else {
        return Err(StarryError::InvalidInput);
    };
    debug!("sys_mprotect <= addr: {addr:#x}, length: {length:x}, prot: {permission_flags:?}");

    if permission_flags.contains(MmapProt::GROWDOWN | MmapProt::GROWSUP) {
        return Err(StarryError::InvalidInput);
    }

    // man 2 mprotect: addr is not a multiple of page size → EINVAL.
    if !addr.is_multiple_of(PAGE_SIZE_4K) {
        return Err(StarryError::InvalidInput);
    }
    // length=0 is a no-op success on Linux.
    if length == 0 {
        return Ok(0);
    }

    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.pin_aspace()?;
    let mut aspace = aspace_arc.lock();
    let length = checked_align_up(length, PAGE_SIZE_4K)?;
    let start_addr = VirtAddr::from(addr);
    let end = start_addr
        .checked_add(length)
        .ok_or(StarryError::NoMemory)?;
    let new_flags: MappingFlags = permission_flags.into();
    let reported_flags = reported_mapping_flags_from_prot(permission_flags);
    let mut cursor = start_addr;
    while cursor < end {
        let Some(fragment) = aspace.next_advice_fragment(cursor, end) else {
            return Err(StarryError::NoMemory);
        };
        if fragment.gap_before {
            // Linux's do_mprotect_pkey() commits each VMA in address order and
            // returns ENOMEM when the next VMA does not begin at the cursor.
            // Earlier fragments deliberately remain published.
            return Err(StarryError::NoMemory);
        }
        let fragment_start = fragment.range.start;
        let fragment_size = fragment.range.size();
        if permission_flags.contains(MmapProt::WRITE) {
            // Capability and seal checks precede this fragment's receipt, but
            // do not roll back already committed Linux-visible prefixes.
            for file in aspace.validate_mprotect_mapping_capabilities(
                fragment_start,
                fragment_size,
                new_flags,
            )? {
                memfd_check_write_seal_for_shared_file_backend(&file)?;
            }
        }
        aspace.protect_with_reported_flags(
            fragment_start,
            fragment_size,
            new_flags,
            reported_flags,
        )?;
        cursor = fragment.range.end;
    }

    Ok(0)
}

const MREMAP_VALID_FLAGS: usize = (MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP) as usize;

fn find_free(
    aspace: &crate::mm::AddrSpace,
    hint: VirtAddr,
    size: usize,
    align: usize,
) -> StarryResult<VirtAddr> {
    let limit = VirtAddrRange::new(aspace.base(), aspace.end());
    aspace
        .find_free_area(hint, size, limit, align)
        .or_else(|| aspace.find_free_area(aspace.base(), size, limit, align))
        .ok_or(StarryError::NoMemory)
}

struct MremapMove<'a> {
    src: VirtAddr,
    src_size: usize,
    target: VirtAddr,
    target_size: usize,
    source: &'a VmaMremapSource,
    huge_page_advice: HugePageAdvice,
    dontunmap: bool,
    src_offset: usize,
    replace_target: bool,
    memlock_limit: Option<MemlockLimit>,
}

struct MremapSourceValidation {
    fragment_count: usize,
    huge_page_advice: HugePageAdvice,
}

/// One Linux `remap_move()` step captured from the immutable source root.
///
/// Linux batches an equal-sized `MREMAP_FIXED` request by moving each VMA in
/// source-address order.  A completed prefix remains moved if a later VMA
/// fails, so each item deliberately owns an independent mutation receipt.
struct FixedMremapFragment {
    source: VmaMremapSource,
    src: VirtAddr,
    target: VirtAddr,
    size: usize,
    source_offset: usize,
}

fn prepare_fixed_mremap_fragments(
    aspace: &crate::mm::AddrSpace,
    src: VirtAddr,
    size: usize,
    target: VirtAddr,
) -> StarryResult<Vec<FixedMremapFragment>> {
    let range = VirtAddrRange::try_from_start_size(src, size)
        .ok_or(StarryError::InvalidInput)?;
    let snapshots = aspace.vma_snapshots_in_range(src, size)?;
    let Some(first) = snapshots.first() else {
        return Err(StarryError::BadAddress);
    };
    // Linux permits gaps between later VMAs and preserves those offsets at
    // the destination, but a gap at old_address is EFAULT.
    if first.range.start > src {
        return Err(StarryError::BadAddress);
    }

    let mut fragments = Vec::new();
    fragments
        .try_reserve(snapshots.len())
        .map_err(|_| StarryError::NoMemory)?;
    for snapshot in snapshots {
        let fragment_start = snapshot.range.start.max(range.start);
        let fragment_end = snapshot.range.end.min(range.end);
        let fragment_size = fragment_end
            .checked_sub_addr(fragment_start)
            .ok_or(StarryError::BadAddress)?;
        if fragment_size == 0 {
            continue;
        }
        let source = aspace
            .mremap_source(fragment_start)
            .ok_or(StarryError::BadAddress)?;
        if source.is_linear() {
            return Err(StarryError::OperationNotSupported);
        }
        let source_offset = fragment_start
            .checked_sub_addr(source.start())
            .ok_or(StarryError::BadAddress)?;
        let target_offset = fragment_start
            .checked_sub_addr(src)
            .ok_or(StarryError::BadAddress)?;
        let fragment_target = target
            .checked_add(target_offset)
            .ok_or(StarryError::InvalidInput)?;
        let alignment = source.alignment();
        if !fragment_start.is_aligned(alignment)
            || !fragment_target.is_aligned(alignment)
            || !fragment_size.is_multiple_of(alignment)
        {
            return Err(StarryError::InvalidInput);
        }
        fragments.push(FixedMremapFragment {
            source,
            src: fragment_start,
            target: fragment_target,
            size: fragment_size,
            source_offset,
        });
    }
    (!fragments.is_empty())
        .then_some(fragments)
        .ok_or(StarryError::BadAddress)
}

fn move_fixed_mremap_fragments(
    aspace: &mut crate::mm::AddrSpace,
    fragments: Vec<FixedMremapFragment>,
) -> StarryResult {
    for fragment in fragments {
        mremap_move(
            aspace,
            MremapMove {
                src: fragment.src,
                src_size: fragment.size,
                target: fragment.target,
                target_size: fragment.size,
                huge_page_advice: fragment.source.huge_page_advice(),
                source: &fragment.source,
                dontunmap: false,
                src_offset: fragment.source_offset,
                replace_target: true,
                memlock_limit: None,
            },
        )?;
    }
    Ok(())
}

/// Validates the complete logical source mapping from the immutable VMA root.
/// A split created by mprotect/THP carving is accepted only when every
/// fragment is contiguous, belongs to one MappingGroup, carries the same
/// permission envelope, and advances its source offset without a gap.
fn validate_mremap_source(
    aspace: &crate::mm::AddrSpace,
    start: VirtAddr,
    size: usize,
) -> StarryResult<MremapSourceValidation> {
    let range = VirtAddrRange::try_from_start_size(start, size)
        .ok_or(StarryError::InvalidInput)?;
    let fragments = aspace.vma_snapshots_in_range(start, size)?;
    let Some(first) = fragments.first() else {
        return Err(StarryError::BadAddress);
    };
    let group_id = first.group.id;
    let source = *first.group.source;
    let page_policy = first.group.page_policy;
    let huge_page_advice = first.huge_page_advice;
    let lock_mode = first.lock_mode;
    let advice_policy = first.advice_policy;
    let rights = first.rights;
    let max_rights = first.max_rights;
    let first_delta = range
        .start
        .checked_sub_addr(first.range.start)
        .ok_or(StarryError::BadAddress)?;
    let mut expected_offset = first
        .source_offset
        .get()
        .checked_add(first_delta)
        .ok_or(StarryError::InvalidInput)?;
    let mut cursor = range.start;

    for fragment in &fragments {
        let fragment_start = fragment.range.start.max(range.start);
        let fragment_end = fragment.range.end.min(range.end);
        if fragment_start != cursor
            || fragment_end <= fragment_start
            || fragment.group.id != group_id
            || fragment.group.source.as_ref() != &source
            || fragment.group.page_policy != page_policy
            || fragment.huge_page_advice != huge_page_advice
            || fragment.lock_mode != lock_mode
            || fragment.advice_policy != advice_policy
            || fragment.rights != rights
            || fragment.max_rights != max_rights
        {
            return Err(StarryError::BadAddress);
        }
        let fragment_delta = fragment_start
            .checked_sub_addr(fragment.range.start)
            .ok_or(StarryError::BadAddress)?;
        let actual_offset = fragment
            .source_offset
            .get()
            .checked_add(fragment_delta)
            .ok_or(StarryError::InvalidInput)?;
        if actual_offset != expected_offset {
            return Err(StarryError::BadAddress);
        }
        let length = fragment_end
            .checked_sub_addr(fragment_start)
            .ok_or(StarryError::BadAddress)?;
        expected_offset = expected_offset
            .checked_add(length)
            .ok_or(StarryError::InvalidInput)?;
        cursor = fragment_end;
    }
    if cursor != range.end {
        return Err(StarryError::BadAddress);
    }
    Ok(MremapSourceValidation {
        fragment_count: fragments.len(),
        huge_page_advice,
    })
}

fn mremap_move(
    aspace: &mut crate::mm::AddrSpace,
    move_args: MremapMove<'_>,
) -> StarryResult {
    let MremapMove {
        src,
        src_size,
        target,
        target_size,
        source,
        huge_page_advice,
        dontunmap,
        src_offset,
        replace_target,
        memlock_limit,
    } = move_args;
    aspace.mremap_move_from_source(
        source,
        src,
        src_size,
        target,
        target_size,
        huge_page_advice,
        dontunmap,
        src_offset,
        replace_target,
        memlock_limit,
    )
}

pub fn sys_mremap(
    addr: usize,
    old_size: usize,
    new_size: usize,
    flags: usize,
    new_addr: usize,
) -> StarryResult<isize> {
    debug!(
        "sys_mremap <= addr: {addr:#x}, old_size: {old_size:x}, new_size: {new_size:x}, flags: \
         {flags:#x}, new_addr: {new_addr:#x}"
    );

    if new_size == 0 {
        return Err(StarryError::InvalidInput);
    }
    if flags & !MREMAP_VALID_FLAGS != 0 {
        return Err(StarryError::InvalidInput);
    }

    let addr = VirtAddr::from(addr);
    if !addr.is_aligned_4k() {
        return Err(StarryError::InvalidInput);
    }
    let may_move = flags & MREMAP_MAYMOVE as usize != 0;
    let fixed = flags & MREMAP_FIXED as usize != 0;
    let dontunmap = flags & MREMAP_DONTUNMAP as usize != 0;

    if (fixed || dontunmap) && !may_move {
        return Err(StarryError::InvalidInput);
    }
    if dontunmap && old_size != new_size {
        return Err(StarryError::InvalidInput);
    }
    if fixed {
        if !new_addr.is_multiple_of(PAGE_SIZE_4K) {
            return Err(StarryError::InvalidInput);
        }
        let old_end = addr
            .as_usize()
            .checked_add(old_size)
            .ok_or(StarryError::InvalidInput)?;
        let new_end = new_addr
            .checked_add(new_size)
            .ok_or(StarryError::InvalidInput)?;
        if old_end > new_addr && new_end > addr.as_usize() {
            return Err(StarryError::InvalidInput);
        }
    }

    let curr = current();
    let memlock_limit = MemlockLimit::for_mapping(
        curr.as_thread().proc_data.rlim.read()[RLIMIT_MEMLOCK].current,
        curr.as_thread().cred().has_cap_ipc_lock(),
    );
    let aspace_ref = curr.as_thread().proc_data.pin_aspace()?;
    let mut aspace = aspace_ref.lock();
    let source = aspace.mremap_source(addr).ok_or(StarryError::BadAddress)?;
    let source_memlock_limit = source.lock_mode().is_locked().then_some(memlock_limit);
    let vma_start = source.start();
    let vma_end = source.end();
    let operation_alignment = source.alignment();
    if !addr.is_aligned(operation_alignment) {
        return Err(StarryError::InvalidInput);
    }
    let old_size = checked_align_up(old_size, operation_alignment)?;
    let new_size = checked_align_up(new_size, operation_alignment)?;
    let src_offset = addr
        .checked_sub_addr(vma_start)
        .ok_or(StarryError::InvalidInput)?;

    if dontunmap && !source.supports_dontunmap() {
        return Err(StarryError::InvalidInput);
    }

    // old_size == 0: duplicate a shared mapping (Linux special case).
    if old_size == 0 {
        let Some(object) = source.shared_object() else {
            return Err(StarryError::InvalidInput);
        };
        if !may_move {
            return Err(StarryError::InvalidInput);
        }
        let shared_size = object
            .capacity_bytes()
            .ok_or(StarryError::InvalidInput)?;
        if src_offset
            .checked_add(new_size)
            .is_none_or(|end| end > shared_size)
        {
            return Err(StarryError::InvalidInput);
        }

        let target = if fixed {
            if !new_addr.is_multiple_of(operation_alignment) {
                return Err(StarryError::InvalidInput);
            }
            let target = VirtAddr::from(new_addr);
            if !aspace.contains_range(target, new_size) {
                return Err(StarryError::NoMemory);
            }
            target
        } else {
            find_free(&aspace, addr, new_size, operation_alignment)?
        };
        drop(object);
        aspace.duplicate_shared_mremap_source(
            &source,
            target,
            new_size,
            src_offset,
            fixed,
            source_memlock_limit,
        )?;
        return Ok(target.as_usize() as isize);
    }

    let old_end = addr
        .checked_add(old_size)
        .ok_or(StarryError::InvalidInput)?;

    if fixed {
        if !new_addr.is_multiple_of(operation_alignment) {
            return Err(StarryError::InvalidInput);
        }
        let target = VirtAddr::from(new_addr);
        if !aspace.contains_range(target, new_size) {
            return Err(StarryError::NoMemory);
        }
        if let Some(target_end) = target.checked_add(new_size)
            && target_end > addr
            && old_end > target
        {
            // The source/target overlap check is repeated after page-size
            // rounding.  The syscall must not destroy the source before this
            // final validation has succeeded.
            return Err(StarryError::InvalidInput);
        }
        if source.is_linear() {
            return Err(StarryError::OperationNotSupported);
        }
        if !dontunmap && old_size == new_size {
            let fragments =
                prepare_fixed_mremap_fragments(&aspace, addr, old_size, target)?;
            move_fixed_mremap_fragments(&mut aspace, fragments)?;
            return Ok(target.as_usize() as isize);
        }
        let source_validation = validate_mremap_source(&aspace, addr, old_size)?;
        mremap_move(
            &mut aspace,
            MremapMove {
                src: addr,
                src_size: old_size,
                target,
                target_size: new_size,
                source: &source,
                huge_page_advice: source_validation.huge_page_advice,
                dontunmap,
                src_offset,
                replace_target: true,
                memlock_limit: source_memlock_limit,
            },
        )?;
        return Ok(target.as_usize() as isize);
    }

    let source_validation = validate_mremap_source(&aspace, addr, old_size)?;

    if new_size == old_size && !dontunmap {
        return Ok(addr.as_usize() as isize);
    }

    if new_size < old_size {
        let tail = addr.checked_add(new_size).ok_or(StarryError::InvalidInput)?;
        aspace.unmap(tail, old_size - new_size)?;
        return Ok(addr.as_usize() as isize);
    }

    if dontunmap {
        let hint = addr.checked_add(old_size).ok_or(StarryError::InvalidInput)?;
        let target = find_free(&aspace, hint, new_size, operation_alignment)?;
        mremap_move(
            &mut aspace,
            MremapMove {
                src: addr,
                src_size: old_size,
                target,
                target_size: new_size,
                source: &source,
                huge_page_advice: source_validation.huge_page_advice,
                dontunmap: true,
                src_offset,
                replace_target: false,
                memlock_limit: source_memlock_limit,
            },
        )?;
        return Ok(target.as_usize() as isize);
    }

    let delta = new_size - old_size;

    let old_end = addr.checked_add(old_size).ok_or(StarryError::InvalidInput)?;
    if source_validation.fragment_count == 1 && old_end == vma_end {
        match aspace.extend_area_with_memlock(addr, delta, source_memlock_limit) {
            Ok(()) => return Ok(addr.as_usize() as isize),
            Err(
                StarryError::NoMemory
                | StarryError::AlreadyExists
                | StarryError::Mapping(MappingError::AlreadyExists),
            ) => {}
            Err(e) => return Err(e),
        }
    }

    if !may_move {
        return Err(StarryError::NoMemory);
    }

    let hint = addr.checked_add(old_size).ok_or(StarryError::InvalidInput)?;
    let target = find_free(&aspace, hint, new_size, operation_alignment)?;
    mremap_move(
        &mut aspace,
        MremapMove {
            src: addr,
            src_size: old_size,
            target,
            target_size: new_size,
            source: &source,
            huge_page_advice: source_validation.huge_page_advice,
            dontunmap: false,
            src_offset,
            replace_target: false,
            memlock_limit: source_memlock_limit,
        },
    )?;
    Ok(target.as_usize() as isize)
}

pub fn sys_madvise(addr: usize, length: usize, advice: i32) -> StarryResult<isize> {
    debug!("sys_madvise <= addr: {addr:#x}, length: {length:x}, advice: {advice:#x}");

    match advice as u32 {
        MADV_NORMAL
        | MADV_RANDOM
        | MADV_SEQUENTIAL
        | MADV_WILLNEED
        | MADV_DONTNEED
        | MADV_FREE
        | MADV_REMOVE
        | MADV_DONTFORK
        | MADV_DOFORK
        | MADV_HUGEPAGE
        | MADV_NOHUGEPAGE
        | MADV_DONTDUMP
        | MADV_DODUMP
        | MADV_PAGEOUT
        | MADV_DONTNEED_LOCKED => {}
        // Recognized Linux advice values without a Starry implementation must
        // be visible to callers.  Returning EOPNOTSUPP prevents an accidental
        // successful no-op from being mistaken for reclaim, THP, or poisoning.
        MADV_WIPEONFORK
        | MADV_KEEPONFORK
        | MADV_MERGEABLE
        | MADV_UNMERGEABLE
        | MADV_COLD
        | MADV_POPULATE_READ
        | MADV_POPULATE_WRITE
        | MADV_COLLAPSE
        | MADV_HWPOISON
        | MADV_SOFT_OFFLINE => return Err(StarryError::OperationNotSupported),
        _ => return Err(StarryError::InvalidInput),
    }

    // man 2 madvise: addr must be page-aligned.
    if !addr.is_multiple_of(PAGE_SIZE_4K) {
        return Err(StarryError::InvalidInput);
    }

    if length == 0 {
        return Ok(0);
    }

    let length = checked_align_up(length, PAGE_SIZE_4K)?;
    let start_va = VirtAddr::from(addr);
    let end = start_va
        .checked_add(length)
        .ok_or(StarryError::InvalidInput)?;
    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.pin_aspace()?;
    let mut cursor = start_va;
    let mut saw_gap = false;

    if advice as u32 == MADV_REMOVE {
        while cursor < end {
            let aspace = aspace_arc.lock();
            let Some(fragment) = aspace.next_advice_fragment(cursor, end) else {
                saw_gap = true;
                break;
            };
            saw_gap |= fragment.gap_before;
            let Some(file_mapping) = fragment.shared_file().cloned() else {
                return Err(StarryError::InvalidInput);
            };
            let fragment_start = fragment.range.start;
            let fragment_len = fragment.range.size();
            let file_offset = file_mapping.file_offset_at(fragment_start);
            // Match Linux madvise_remove(): pin the file/backend snapshot,
            // drop mmap metadata protection, then enter the filesystem.  A
            // later VMA failure intentionally preserves this prefix.
            drop(aspace);
            crate::file::memfd::punch_shared_file_backend(
                &file_mapping,
                file_offset,
                fragment_len,
            )?;
            cursor = fragment_start
                .checked_add(fragment_len)
                .ok_or(StarryError::InvalidInput)?;
        }
    } else if advice as u32 == MADV_PAGEOUT {
        // Linux treats PAGEOUT as a best-effort LRU reclaim request.  Starry
        // currently has a typed eviction path only for disk-backed file-cache
        // PageObjects: it snapshots rmap, drops cache/aspace locks, clears each
        // PTE through that address space's receipt, waits for TLB retirement,
        // and only then releases the clean cache owner.  Anonymous/private and
        // tmpfs mappings would require swap ownership; report that missing
        // capability instead of discarding their contents or claiming a no-op
        // success.
        while cursor < end {
            let aspace = aspace_arc.lock();
            let Some(fragment) = aspace.next_advice_fragment(cursor, end) else {
                saw_gap = true;
                break;
            };
            saw_gap |= fragment.gap_before;
            let fragment_end = fragment.range.end;
            // Rmap-driven eviction pins and mutates the target address space,
            // so never enter it while holding the VMA publication lock.
            drop(aspace);
            fragment.pageout()?;
            cursor = fragment_end;
        }
    } else if advice as u32 == MADV_WILLNEED {
        // With CONFIG_SWAP disabled Linux returns EBADF for anonymous private
        // VMAs. File and shmem prefetch require a backend reservation API that
        // Starry does not yet expose, so keep that capability explicit instead
        // of pretending that the hint was consumed.
        let aspace = aspace_arc.lock();
        let Some(fragment) = aspace.next_advice_fragment(cursor, end) else {
            return Err(StarryError::NoMemory);
        };
        if fragment.gap_before {
            return Err(StarryError::NoMemory);
        }
        if fragment.is_private_anonymous() {
            return Err(StarryError::BadFileDescriptor);
        }
        return Err(StarryError::OperationNotSupported);
    } else if matches!(
        advice as u32,
        MADV_NORMAL
            | MADV_RANDOM
            | MADV_SEQUENTIAL
            | MADV_DONTFORK
            | MADV_DOFORK
            | MADV_DONTDUMP
            | MADV_DODUMP
    ) {
        let update = match advice as u32 {
            MADV_NORMAL => VmaAdviceUpdate::AccessPattern(VmaAccessPattern::Normal),
            MADV_RANDOM => VmaAdviceUpdate::AccessPattern(VmaAccessPattern::Random),
            MADV_SEQUENTIAL => VmaAdviceUpdate::AccessPattern(VmaAccessPattern::Sequential),
            MADV_DONTFORK => VmaAdviceUpdate::DontFork(true),
            MADV_DOFORK => VmaAdviceUpdate::DontFork(false),
            MADV_DONTDUMP => VmaAdviceUpdate::DontDump(true),
            MADV_DODUMP => VmaAdviceUpdate::DontDump(false),
            _ => unreachable!(),
        };
        let reject_special = matches!(advice as u32, MADV_DOFORK | MADV_DODUMP);
        let mut aspace = aspace_arc.lock();
        while cursor < end {
            let Some(fragment) = aspace.next_advice_fragment(cursor, end) else {
                saw_gap = true;
                break;
            };
            saw_gap |= fragment.gap_before;
            if reject_special && fragment.is_special() {
                return Err(StarryError::InvalidInput);
            }
            let fragment_start = fragment.range.start;
            let fragment_len = fragment.range.size();
            aspace.advise_vma_policy(fragment_start, fragment_len, update)?;
            cursor = fragment_start
                .checked_add(fragment_len)
                .ok_or(StarryError::InvalidInput)?;
        }
    } else if matches!(advice as u32, MADV_HUGEPAGE | MADV_NOHUGEPAGE) {
        // Linux updates VM_HUGEPAGE/VM_NOHUGEPAGE one VMA fragment at a
        // time under the mmap write lock.  Gaps are remembered as ENOMEM,
        // while already updated prefixes and later mapped suffixes remain
        // committed.
        let policy = if advice as u32 == MADV_HUGEPAGE {
            HugePageAdvice::Prefer
        } else {
            HugePageAdvice::Avoid
        };
        let mut aspace = aspace_arc.lock();
        while cursor < end {
            let Some(fragment) = aspace.next_advice_fragment(cursor, end) else {
                saw_gap = true;
                break;
            };
            saw_gap |= fragment.gap_before;
            let fragment_start = fragment.range.start;
            let fragment_len = fragment.range.size();
            aspace.advise_huge_pages(fragment_start, fragment_len, policy)?;
            cursor = fragment_start
                .checked_add(fragment_len)
                .ok_or(StarryError::InvalidInput)?;
        }
    } else {
        // DONTNEED/FREE keep the address-space metadata stable for the whole
        // walk, corresponding to Linux's mmap/VMA read-side lock.  Each
        // fragment is still its own page-table mutation, so a later error
        // preserves already applied prefix effects.
        let mut aspace = aspace_arc.lock();
        while cursor < end {
            let Some(fragment) = aspace.next_advice_fragment(cursor, end) else {
                saw_gap = true;
                break;
            };
            saw_gap |= fragment.gap_before;
            let fragment_start = fragment.range.start;
            let fragment_len = fragment.range.size();
            if fragment.is_locked() && advice as u32 != MADV_DONTNEED_LOCKED {
                return Err(StarryError::InvalidInput);
            }
            if matches!(advice as u32, MADV_DONTNEED | MADV_DONTNEED_LOCKED) {
                // Linux zaps each VMA fragment in address order.  A later
                // invalid VMA or gap does not roll back this prefix.
                aspace.discard_range(fragment_start, fragment_len)?;
            } else {
                // Linux madvise_free_single_vma(): only private anonymous
                // mappings are eligible; validation is per VMA immediately
                // before its page-table walk.
                if !fragment.is_private_anonymous() {
                    return Err(StarryError::InvalidInput);
                }
                aspace.mark_lazy_free(fragment_start, fragment_len)?;
            }
            cursor = fragment_start
                .checked_add(fragment_len)
                .ok_or(StarryError::InvalidInput)?;
        }
    }

    if saw_gap {
        return Err(StarryError::NoMemory);
    }

    Ok(0)
}

pub fn sys_msync(addr: usize, length: usize, flags: u32) -> StarryResult<isize> {
    debug!("sys_msync <= addr: {addr:#x}, length: {length:x}, flags: {flags:#x}");

    if !addr.is_multiple_of(PAGE_SIZE_4K) {
        return Err(StarryError::InvalidInput);
    }

    let valid_flags = MS_SYNC | MS_ASYNC | MS_INVALIDATE;
    if flags & !valid_flags != 0 {
        return Err(StarryError::InvalidInput);
    }
    if flags & MS_SYNC != 0 && flags & MS_ASYNC != 0 {
        return Err(StarryError::InvalidInput);
    }
    if length == 0 {
        return Ok(0);
    }

    let rounded_length = checked_align_up(length, PAGE_SIZE_4K)?;
    let start = VirtAddr::from(addr);
    let end_val = addr
        .checked_add(rounded_length)
        .ok_or(StarryError::InvalidInput)?;
    let end = VirtAddr::from(end_val);

    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.pin_aspace()?;
    let mut cursor = start;
    let mut saw_gap = false;
    while cursor < end {
        let aspace = aspace_arc.lock();
        let Some(fragment) = aspace.next_advice_fragment(cursor, end) else {
            saw_gap = true;
            break;
        };
        saw_gap |= fragment.gap_before;
        let range_start = fragment.range.start;
        let range_end = fragment.range.end;
        // Linux returns ENOMEM immediately for an asynchronous request that
        // reaches a hole. Other modes remember the hole and may still be
        // superseded by EBUSY on a later VM_LOCKED VMA.
        if fragment.gap_before && flags == MS_ASYNC {
            return Err(StarryError::NoMemory);
        }
        if flags & MS_INVALIDATE != 0 && fragment.is_locked() {
            return Err(StarryError::ResourceBusy);
        }
        let sync_backend = (flags & MS_SYNC != 0)
            .then(|| fragment.shared_file().cloned())
            .flatten();
        // Linux drops mmap_lock before vfs_fsync_range().  The cloned backend
        // pins the file/cache identity while VMA metadata is unlocked.
        drop(aspace);
        if let Some(file_backend) = sync_backend {
            file_backend.writeback_range(range_start, range_end)?;
        }
        cursor = range_end;
    }

    if saw_gap {
        return Err(StarryError::NoMemory);
    }

    Ok(0)
}

pub fn sys_mlock(addr: usize, length: usize) -> StarryResult<isize> {
    sys_mlock2(addr, length, 0)
}

pub fn sys_mlock2(addr: usize, length: usize, flags: u32) -> StarryResult<isize> {
    // Linux `mlock2` accepts only `flags == 0` or `MLOCK_ONFAULT`; any other bit
    // is rejected with EINVAL and must produce no populate/fault side effect.
    const MLOCK_ONFAULT: u32 = 0x01;
    if flags & !MLOCK_ONFAULT != 0 {
        return Err(StarryError::InvalidInput);
    }
    if length == 0 {
        return Ok(0);
    }
    let aligned = addr.align_down(PAGE_SIZE_4K);
    // `checked_add` guards `addr + length`, but `align_up` itself adds
    // `PAGE_SIZE - 1` internally and can still wrap a near-`usize::MAX` end to a
    // small value; detect that wrap (end < raw_end) and reject, as Linux rejects
    // an out-of-range mlock with EINVAL rather than locking a tiny wrapped range.
    let raw_end = addr.checked_add(length).ok_or(StarryError::InvalidInput)?;
    let end = checked_align_up(raw_end, PAGE_SIZE_4K)?;
    let size = end - aligned;

    let curr = current();
    let memlock_limit = MemlockLimit::for_mlock(
        curr.as_thread().proc_data.rlim.read()[RLIMIT_MEMLOCK].current,
        curr.as_thread().cred().has_cap_ipc_lock(),
    );
    if !memlock_limit.can_lock() {
        return Err(StarryError::OperationNotPermitted);
    }
    let aspace_arc = curr.as_thread().proc_data.pin_aspace()?;
    let mut aspace = aspace_arc.lock();
    let start = VirtAddr::from(aligned);
    let lock_mode = if flags & MLOCK_ONFAULT != 0 {
        VmaLockMode::LockOnFault
    } else {
        VmaLockMode::Locked
    };
    // Linux applies VM_LOCKED/VM_LOCKONFAULT under mmap_write_lock before
    // optional population. A later fault-in failure therefore leaves the VMA
    // policy visible rather than pretending the metadata transaction aborted.
    aspace.lock_vma_range(start, size, lock_mode, memlock_limit)?;
    if lock_mode == VmaLockMode::Locked {
        // Plain mlock (flags == 0): honor the "fault now" contract by faulting
        // the whole range in, reporting ENOMEM on any unmapped page. On this
        // no-swap kernel the faulted pages then stay resident, satisfying mlock's
        // residency guarantee. `populate_area` is the MAP_POPULATE primitive.
        aspace.populate_area(start, size, MappingFlags::READ)?;
    }
    Ok(0)
}

pub fn sys_munlock(addr: usize, length: usize) -> StarryResult<isize> {
    if length == 0 {
        return Ok(0);
    }
    let aligned = addr.align_down(PAGE_SIZE_4K);
    let raw_end = addr.checked_add(length).ok_or(StarryError::InvalidInput)?;
    let end = checked_align_up(raw_end, PAGE_SIZE_4K)?;
    let size = end
        .checked_sub(aligned)
        .ok_or(StarryError::InvalidInput)?;

    let curr = current();
    let aspace_arc = curr.as_thread().proc_data.pin_aspace()?;
    aspace_arc
        .lock()
        .unlock_vma_range(VirtAddr::from(aligned), size)?;
    Ok(0)
}

#[cfg(all(test, not(axtest)))]
fn mmap_capped_device_map_len_rules_hold_for_test() -> bool {
    // capped_device_map_len: returns min of request and aligned available.
    let page_size = PAGE_SIZE_4K;
    assert_eq!(capped_device_map_len(1000, 4096, page_size).unwrap(), 1000); // request < available
    assert_eq!(capped_device_map_len(8192, 4096, page_size).unwrap(), 4096); // request > available
    assert_eq!(capped_device_map_len(0, 8192, page_size).unwrap(), 0); // zero request
    assert_eq!(capped_device_map_len(5000, 4096, page_size).unwrap(), 4096); // request > available (aligned)
    assert!(checked_align_up(usize::MAX, page_size).is_err());
    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn mmap_capped_device_map_len_rules_hold() {
        assert!(super::mmap_capped_device_map_len_rules_hold_for_test());
    }
}
