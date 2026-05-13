use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

use ax_errno::{AxError, AxResult};
use ax_fs::{FS_CONTEXT, FileFlags, OpenOptions};
use ax_hal::paging::MappingFlags;
use ax_io::{Seek, SeekFrom};
use ax_memory_addr::VirtAddr;
use ax_memory_set::MemoryArea;
use linux_raw_sys::general::{
    F_SEAL_GROW, F_SEAL_SEAL, F_SEAL_SHRINK, F_SEAL_WRITE, MFD_CLOEXEC, O_RDWR,
};

use crate::{
    file::{File, FileLike},
    mm::{AddrSpace, Backend, UserConstPtr},
    pseudofs,
};

// Linux: only MFD_CLOEXEC and MFD_ALLOW_SEALING are defined today.
const MFD_ALLOW_SEALING: u32 = 2;
const MFD_KNOWN_FLAGS: u32 = MFD_CLOEXEC | MFD_ALLOW_SEALING;

/// Linux `memfd_create(2)`: name length limit excluding the terminating NUL.
const MFD_NAME_MAX_LEN: usize = 249;

#[derive(Debug)]
pub struct MemFdMeta {
    pub allow_sealing: bool,
    pub seals: AtomicU32,
    /// Number of `MAP_SHARED` + writable file-backed VMAs for this memfd inode.
    /// Used for O(1) `F_ADD_SEALS(F_SEAL_WRITE)` busy checks (see
    /// `memfd_on_aspace_unmap_range` / `memfd_on_after_map`).
    ///
    /// Linux also has `memfd_wait_for_pins` (GUP / folio pins) before sealing;
    /// StarryOS does not model that path yet, so this counter only tracks
    /// user-visible VMAs.
    pub shared_writable_mmap_count: AtomicU32,
}

pub fn memfd_seals_for_file_like(file_like: &Arc<dyn FileLike>) -> Option<u32> {
    let file = file_like.downcast_ref::<File>()?;
    let loc = file.inner().backend().ok()?.location();
    let meta = loc.user_data().get::<MemFdMeta>()?;
    Some(meta.seals.load(core::sync::atomic::Ordering::Relaxed))
}

pub fn memfd_check_write_seal(file_like: &Arc<dyn FileLike>) -> AxResult<()> {
    let Some(seals) = memfd_seals_for_file_like(file_like) else {
        return Ok(());
    };
    if seals & F_SEAL_WRITE != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(())
}

/// Like [`memfd_check_write_seal`], but keyed off a mapped [`Backend::File`] (no `FileLike` handle).
///
/// Used by `mprotect` when raising `PROT_WRITE` on an existing shared file-backed VMA
/// (Linux forbids making a sealed memfd mapping writable through `mprotect`, not only
/// through `mmap`).
pub fn memfd_check_write_seal_for_shared_file_backend(backend: &Backend) -> AxResult<()> {
    let Backend::File(f) = backend else {
        return Ok(());
    };
    if !f.is_shared_file_map() {
        return Ok(());
    };
    let Some(meta) = f.cache_location().user_data().get::<MemFdMeta>() else {
        return Ok(());
    };
    if meta.seals.load(Ordering::Relaxed) & F_SEAL_WRITE != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(())
}

pub fn memfd_check_resize_seals(
    file_like: &Arc<dyn FileLike>,
    old_len: u64,
    new_len: u64,
) -> AxResult<()> {
    let Some(seals) = memfd_seals_for_file_like(file_like) else {
        return Ok(());
    };
    if new_len > old_len && (seals & F_SEAL_GROW != 0) {
        return Err(AxError::OperationNotPermitted);
    }
    if new_len < old_len && (seals & F_SEAL_SHRINK != 0) {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(())
}

pub fn memfd_check_implicit_grow_seal(
    file_like: &Arc<dyn FileLike>,
    old_len: u64,
    end_offset: u64,
) -> AxResult<()> {
    let Some(seals) = memfd_seals_for_file_like(file_like) else {
        return Ok(());
    };
    if end_offset > old_len && (seals & F_SEAL_GROW != 0) {
        return Err(AxError::OperationNotPermitted);
    }
    Ok(())
}

/// Memfd seal checks before a seekable write at the file's current offset (`write` / `writev`).
pub fn memfd_checks_before_stream_write(
    file_like: &Arc<dyn FileLike>,
    write_len: u64,
) -> AxResult<()> {
    memfd_check_write_seal(file_like)?;
    if write_len == 0 {
        return Ok(());
    }
    let Some(file) = file_like.downcast_ref::<File>() else {
        return Ok(());
    };
    let old_len = file.inner().location().len()?;
    let pos = if file.inner().access(FileFlags::APPEND).is_ok() {
        old_len
    } else {
        file.inner().seek(SeekFrom::Current(0))?
    };
    let end = pos.saturating_add(write_len);
    memfd_check_implicit_grow_seal(file_like, old_len, end)?;
    Ok(())
}

/// Memfd seal checks before `pwrite` / `write_at` at a fixed byte offset.
pub fn memfd_checks_before_write_at(
    file_like: &Arc<dyn FileLike>,
    offset: u64,
    write_len: u64,
) -> AxResult<()> {
    memfd_check_write_seal(file_like)?;
    if write_len == 0 {
        return Ok(());
    }
    let Some(file) = file_like.downcast_ref::<File>() else {
        return Ok(());
    };
    let old_len = file.inner().location().len()?;
    let end = offset.saturating_add(write_len);
    memfd_check_implicit_grow_seal(file_like, old_len, end)?;
    Ok(())
}

/// Returns `true` if `F_ADD_SEALS(F_SEAL_WRITE)` must fail with `EBUSY` because
/// this memfd still has at least one shared writable mapping.
pub(crate) fn memfd_shared_writable_seal_is_busy(meta: &MemFdMeta) -> bool {
    meta.shared_writable_mmap_count.load(Ordering::SeqCst) > 0
}

fn memfd_meta_for_file_backend(backend: &Backend) -> Option<Arc<MemFdMeta>> {
    let Backend::File(f) = backend else {
        return None;
    };
    if !f.is_shared_file_map() {
        return None;
    }
    f.cache_location().user_data().get::<MemFdMeta>()
}

pub(crate) fn memfd_memory_area_is_shared_writable_memfd(area: &MemoryArea<Backend>) -> bool {
    if !area.flags().contains(MappingFlags::WRITE) {
        return false;
    }
    memfd_meta_for_file_backend(area.backend()).is_some()
}

fn memfd_apply_shared_writable_count_delta(meta: &MemFdMeta, delta: i32) {
    if delta > 0 {
        meta.shared_writable_mmap_count
            .fetch_add(delta as u32, Ordering::SeqCst);
    } else if delta < 0 {
        let sub = (-delta) as u32;
        loop {
            let cur = meta.shared_writable_mmap_count.load(Ordering::SeqCst);
            if cur < sub {
                warn!(
                    "memfd shared_writable_mmap_count underflow (cur={cur}, sub={sub}); leaving \
                     counter unchanged — accounting bug suspected"
                );
                debug_assert!(
                    cur >= sub,
                    "memfd shared_writable_mmap_count underflow (cur={cur}, sub={sub})"
                );
                break;
            }
            let next = cur - sub;
            if meta
                .shared_writable_mmap_count
                .compare_exchange_weak(cur, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }
    }
}

pub(crate) fn memfd_apply_shared_writable_delta_for_backend(
    backend: &Backend,
    old_flags: MappingFlags,
    new_flags: MappingFlags,
) {
    let Some(meta) = memfd_meta_for_file_backend(backend) else {
        return;
    };
    let old_q = old_flags.contains(MappingFlags::WRITE);
    let new_q = new_flags.contains(MappingFlags::WRITE);
    let delta = new_q as i32 - old_q as i32;
    memfd_apply_shared_writable_count_delta(meta.as_ref(), delta);
}

/// After a successful [`AddrSpace::map`], bump the counter if the new area is a
/// shared writable memfd file mapping.
pub(crate) fn memfd_on_after_map(aspace: &AddrSpace, start: VirtAddr) {
    let Some(area) = aspace.find_area(start) else {
        return;
    };
    if !memfd_memory_area_is_shared_writable_memfd(area) {
        return;
    }
    let Backend::File(f) = area.backend() else {
        return;
    };
    let Some(meta) = f.cache_location().user_data().get::<MemFdMeta>() else {
        return;
    };
    memfd_apply_shared_writable_count_delta(meta.as_ref(), 1);
}

/// Before [`AddrSpace::unmap`] / [`AddrSpace::unmap_metadata`], adjust counters
/// for memfd-backed shared writable VMAs affected by this range (mirrors the
/// `MemorySet::unmap` split / shrink / full-remove geometry).
///
/// This assumes [`ax_memory_set::MemorySet::unmap`] does **not** coalesce adjacent
/// VMAs after a partial unmap. If coalescing is ever added, memfd accounting must
/// be updated at the merge site (or switched to a post-merge resync).
pub(crate) fn memfd_on_aspace_unmap_range(aspace: &AddrSpace, ustart: VirtAddr, ulen: usize) {
    let uend = ustart + ulen;
    for area in aspace.areas() {
        let a0 = area.start();
        let a1 = area.end();
        if a1 <= ustart || a0 >= uend {
            continue;
        }
        let Backend::File(f) = area.backend() else {
            continue;
        };
        if !f.is_shared_file_map() {
            continue;
        };
        let Some(meta) = f.cache_location().user_data().get::<MemFdMeta>() else {
            continue;
        };
        if !area.flags().contains(MappingFlags::WRITE) {
            continue;
        }

        if ustart <= a0 && uend >= a1 {
            memfd_apply_shared_writable_count_delta(meta.as_ref(), -1);
        } else if ustart > a0 && uend < a1 {
            // Strict interior unmap splits one VMA into two shared-writable maps.
            memfd_apply_shared_writable_count_delta(meta.as_ref(), 1);
        }
    }
}

/// Collect distinct [`MemFdMeta`] handles for memfd-backed **shared** file mappings
/// that overlap `[ustart, ustart + ulen)` before [`AddrSpace::protect`].
///
/// Used so that after `MemorySet::protect` (which may **split** one VMA into
/// several), we can **rescan** the address space and set
/// [`MemFdMeta::shared_writable_mmap_count`] to the true VMA count (delta-only
/// updates are insufficient across interior splits).
pub(crate) fn memfd_collect_metas_touching_mprotect_range(
    aspace: &AddrSpace,
    ustart: VirtAddr,
    ulen: usize,
) -> Vec<Arc<MemFdMeta>> {
    let uend = ustart + ulen;
    let mut metas = Vec::new();
    for area in aspace.areas() {
        if area.end() <= ustart || area.start() >= uend {
            continue;
        }
        let Backend::File(f) = area.backend() else {
            continue;
        };
        if !f.is_shared_file_map() {
            continue;
        }
        let Some(m) = f.cache_location().user_data().get::<MemFdMeta>() else {
            continue;
        };
        if !metas.iter().any(|x: &Arc<MemFdMeta>| Arc::ptr_eq(x, &m)) {
            metas.push(m);
        }
    }
    metas
}

/// After a successful [`AddrSpace::protect`], set each touched memfd's
/// `shared_writable_mmap_count` to the number of live shared-writable VMAs for
/// that inode (same `Arc<MemFdMeta>` identity via [`Arc::ptr_eq`]).
pub(crate) fn memfd_resync_shared_writable_counts_after_mprotect(
    aspace: &AddrSpace,
    touched: &[Arc<MemFdMeta>],
) {
    for meta in touched {
        let mut count: u32 = 0;
        for area in aspace.areas() {
            if !memfd_memory_area_is_shared_writable_memfd(area) {
                continue;
            }
            let Backend::File(f) = area.backend() else {
                continue;
            };
            let Some(m) = f.cache_location().user_data().get::<MemFdMeta>() else {
                continue;
            };
            if Arc::ptr_eq(&m, meta) {
                count = count.saturating_add(1);
            }
        }
        meta.shared_writable_mmap_count
            .store(count, Ordering::SeqCst);
    }
}

/// When tearing down an entire address space, subtract every live memfd
/// shared-writable VMA once (pairs with per-map increments).
pub(crate) fn memfd_release_all_shared_writable_counts_for_aspace(aspace: &AddrSpace) {
    for area in aspace.areas() {
        if !memfd_memory_area_is_shared_writable_memfd(area) {
            continue;
        }
        let Backend::File(f) = area.backend() else {
            continue;
        };
        let Some(meta) = f.cache_location().user_data().get::<MemFdMeta>() else {
            continue;
        };
        memfd_apply_shared_writable_count_delta(meta.as_ref(), -1);
    }
}

/// Before [`AddrSpace::replace_area_metadata`], transition memfd shared-writable
/// accounting from the overlapped VMA(s) to the replacement metadata (used by
/// `mremap(..., MREMAP_DONTUNMAP)` when the source mapping is replaced by anon).
///
/// Callers must pass a range that [`ax_memory_set::MemorySet::replace_area_metadata`]
/// accepts: a single contiguous span within one existing VMA. If file→file
/// replacement is ever introduced, avoid double-counting with [`memfd_on_after_map`].
pub(crate) fn memfd_on_aspace_replace_metadata(
    aspace: &AddrSpace,
    ustart: VirtAddr,
    ulen: usize,
    new_flags: MappingFlags,
    new_backend: &Backend,
) {
    let empty = MappingFlags::empty();
    for (_frag_start, _frag_size, old_flags, old_backend) in aspace.areas_in_range(ustart, ulen) {
        memfd_apply_shared_writable_delta_for_backend(&old_backend, old_flags, empty);
    }
    memfd_apply_shared_writable_delta_for_backend(new_backend, empty, new_flags);
}

pub fn sys_memfd_create(name: UserConstPtr<core::ffi::c_char>, flags: u32) -> AxResult<isize> {
    if flags & !MFD_KNOWN_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }
    if name.is_null() {
        return Err(AxError::BadAddress);
    }
    let name_str = name.get_as_str()?;
    if name_str.len() > MFD_NAME_MAX_LEN {
        return Err(AxError::InvalidInput);
    }

    // Create an unlinked tmpfs inode directly, without any pathname.
    // This mirrors Linux's shmem-backed memfd: inode exists and is kept alive
    // only by the returned fd.
    let (mount_path, tmpfs) = if fs_has_dir("/dev/shm") {
        ("/dev/shm", pseudofs::shm_tmpfs())
    } else {
        ("/tmp", pseudofs::tmp_tmpfs())
    };
    let Some(tmpfs) = tmpfs else {
        // Pseudofs should have mounted tmpfs already; treat missing handle as
        // a hard failure rather than falling back to path-visible files.
        return Err(AxError::NotFound);
    };

    let fs = FS_CONTEXT.lock();
    let shm_loc = fs.resolve(mount_path)?;
    let mountpoint = shm_loc.mountpoint().clone();

    let entry = tmpfs.create_anonymous_file(
        name_str,
        axfs_ng_vfs::NodePermission::from_bits_truncate(0o666),
    );
    let loc = axfs_ng_vfs::Location::new(mountpoint, entry);
    let allow_sealing = flags & MFD_ALLOW_SEALING != 0;
    // Linux: without MFD_ALLOW_SEALING the memfd is born with F_SEAL_SEAL set so
    // F_ADD_SEALS cannot be used (F_GET_SEALS reflects this).
    let initial_seals = if allow_sealing { 0 } else { F_SEAL_SEAL };
    loc.user_data().insert(MemFdMeta {
        allow_sealing,
        seals: AtomicU32::new(initial_seals),
        shared_writable_mmap_count: AtomicU32::new(0),
    });

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open_loc(loc)?
        .into_file()?;

    let cloexec = flags & MFD_CLOEXEC != 0;
    File::new(file, O_RDWR)
        .add_to_fd_table(cloexec)
        .map(|fd| fd as _)
}

fn fs_has_dir(path: &str) -> bool {
    FS_CONTEXT.lock().resolve(path).is_ok()
}
