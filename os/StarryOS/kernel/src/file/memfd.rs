//! `memfd_create` backing object.
//!
//! A `Memfd` wraps a regular tmpfs-backed `File` and adds a per-fd seal mask
//! so userspace can call `fcntl(F_ADD_SEALS, F_GET_SEALS)` and see Linux
//! semantics. The backing file is an anonymous tmpfs inode, so it has no
//! directory entry and `fstat(fd).st_nlink == 0` matches Linux's anonymous-inode
//! model.
//!
//! Seals tracked:
//!   - `F_SEAL_SEAL`    — no further seals allowed
//!   - `F_SEAL_SHRINK`  — file size cannot shrink (enforced in ftruncate)
//!   - `F_SEAL_GROW`    — file size cannot grow   (enforced in ftruncate)
//!   - `F_SEAL_WRITE`   — no further writes via write(); also rejects new
//!     `MAP_SHARED|PROT_WRITE` mmap calls; adding it fails with `EBUSY`
//!     while any live shared writable mapping exists
//!   - `F_SEAL_FUTURE_WRITE` — like `F_SEAL_WRITE` for every future write
//!     path (write/pwrite/writev, fallocate, new `MAP_SHARED|PROT_WRITE`
//!     mmap), but mappings created before the seal keep write access, so
//!     adding it never returns `EBUSY`
//!
//! Wayland's `wl_shm` requires `F_SEAL_SHRINK`, which is fully enforced;
//! Chromium/Firefox seal read-only shared-memory snapshots with
//! `F_SEAL_FUTURE_WRITE` after populating them.

use alloc::{
    borrow::Cow, collections::BTreeMap, format, string::String, sync::Arc, vec::Vec,
};
use core::{
    sync::atomic::{AtomicU32, Ordering},
    task::Context,
};

use ax_fs_ng::vfs::FileFlags;
use ax_io::{IoBuf, SeekFrom, prelude::*};
use ax_memory_addr::{MemoryAddr, VirtAddr};
use ax_runtime::hal::paging::MappingFlags;
use axfs_ng_vfs::FileRangeOperation;
use axpoll::{IoEvents, Pollable};

use super::{File, FileLike, IoDst, IoSrc, Kstat, get_file_like};
use crate::{
    StarryError, StarryResult,
    mm::{
        AddrSpace, AddressSpaceId, MappingOperation, SharedFileMappingLease,
        SharedFileVmaRecord, is_address_space_live,
    },
    sync::Mutex,
};

pub const F_SEAL_SEAL: u32 = 0x0001;
pub const F_SEAL_SHRINK: u32 = 0x0002;
pub const F_SEAL_GROW: u32 = 0x0004;
pub const F_SEAL_WRITE: u32 = 0x0008;
pub const F_SEAL_FUTURE_WRITE: u32 = 0x0010;

/// Mask of bits that can ever appear in a seal mask.
pub const F_SEAL_ALL: u32 =
    F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_FUTURE_WRITE;

/// Seals that reject a fresh write, matching Linux's
/// `seals & (F_SEAL_WRITE | F_SEAL_FUTURE_WRITE)` guard in `mm/shmem.c`.
/// `F_SEAL_FUTURE_WRITE` blocks the same write paths as `F_SEAL_WRITE`;
/// the two only differ in whether adding the seal tolerates pre-existing
/// shared writable mappings (handled in `add_seals`).
pub const F_SEAL_ANY_WRITE: u32 = F_SEAL_WRITE | F_SEAL_FUTURE_WRITE;

#[derive(Clone)]
pub struct MemfdRef(pub Arc<Memfd>);

pub struct Memfd {
    inner: Arc<File>,
    seals: AtomicU32,
    /// Writable shared VMA fragments grouped by their owning address space.
    ///
    /// The lifecycle registry decides whether an owner is still Linux-visible;
    /// a retiring MM therefore stops blocking `F_SEAL_WRITE` before its PTE
    /// frames are asynchronously reclaimed.  This mirrors Linux `exit_mmap()`
    /// without making a CPU activation or a temporary kernel pin look like a
    /// userspace mapping owner.
    shared_writable_mmaps: Mutex<BTreeMap<AddressSpaceId, u32>>,
    /// Userspace-visible name (the `name` arg to `memfd_create`). Included
    /// in the reported path so `/proc/*/fd/*` matches Linux's
    /// `/memfd:<name>` convention.
    name: String,
    /// Serializes seal-check-and-truncate to close the TOCTOU window
    /// between `check_truncate` and the underlying `set_len`.
    truncate_mtx: Mutex<()>,
}

impl Memfd {
    /// Build a Memfd around an already-open backing file.
    ///
    /// `allow_sealing` — when false, `F_SEAL_SEAL` is set immediately so any
    /// `F_ADD_SEALS` fails with `EPERM`, matching Linux behavior for
    /// `memfd_create` without `MFD_ALLOW_SEALING`.
    pub fn new(inner: Arc<File>, name: String, allow_sealing: bool) -> Arc<Self> {
        let initial = if allow_sealing { 0 } else { F_SEAL_SEAL };
        Arc::new(Self {
            inner,
            seals: AtomicU32::new(initial),
            shared_writable_mmaps: Mutex::new(BTreeMap::new()),
            name,
            truncate_mtx: Mutex::new(()),
        })
    }

    pub fn inner(&self) -> &Arc<File> {
        &self.inner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn get_seals(&self) -> u32 {
        self.seals.load(Ordering::Acquire)
    }

    pub fn check_write_seal(&self) -> StarryResult {
        if self.get_seals() & F_SEAL_ANY_WRITE != 0 {
            Err(StarryError::OperationNotPermitted)
        } else {
            Ok(())
        }
    }

    /// Add the given seals to the current set. Returns `OperationNotPermitted`
    /// if `F_SEAL_SEAL` is already set (so the mask is frozen), or
    /// `InvalidInput` if the requested seal bits are outside the supported
    /// mask.
    pub fn add_seals(&self, add: u32) -> StarryResult {
        if add & !F_SEAL_ALL != 0 {
            return Err(StarryError::InvalidInput);
        }
        // Hold `truncate_mtx` across the seal publish so in-flight
        // `set_len_sealed` calls either finish before we set the seal (and
        // their check_truncate saw the pre-seal mask) or start after we
        // set it (and see the new mask). Without this, a concurrent
        // ftruncate could pass its seal check with the pre-seal mask,
        // call set_len, and materialize a shrink/grow the seal was
        // intended to forbid. Linux's memfd_fcntl takes inode_lock here
        // for the same reason.
        let _trunc = self.truncate_mtx.lock();
        let mut prev = self.seals.load(Ordering::Acquire);
        loop {
            if prev & F_SEAL_SEAL != 0 {
                return Err(StarryError::OperationNotPermitted);
            }
            if add & F_SEAL_WRITE != 0
                && prev & F_SEAL_WRITE == 0
                && self
                    .shared_writable_mmaps
                    .lock()
                    .iter()
                    .any(|(mm_id, count)| *count != 0 && is_address_space_live(*mm_id))
            {
                return Err(StarryError::ResourceBusy);
            }
            let new = prev | add;
            match self
                .seals
                .compare_exchange_weak(prev, new, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
        Ok(())
    }

    /// Check `F_SEAL_SHRINK`/`F_SEAL_GROW` against a proposed new size.
    /// Returns `Err(OperationNotPermitted)` if the operation is disallowed.
    fn check_truncate(&self, current_len: u64, new_len: u64) -> StarryResult {
        let seals = self.get_seals();
        if new_len < current_len && seals & F_SEAL_SHRINK != 0 {
            return Err(StarryError::OperationNotPermitted);
        }
        if new_len > current_len && seals & F_SEAL_GROW != 0 {
            return Err(StarryError::OperationNotPermitted);
        }
        Ok(())
    }

    /// Seal-aware `ftruncate`. Holds `truncate_mtx` across the length
    /// query, seal check, and underlying `set_len` to close the TOCTOU
    /// window: without this lock, two concurrent `ftruncate` calls could
    /// both read the pre-shrink size, both pass `check_truncate`, and
    /// both race on `set_len`, with only the last write observed.
    pub fn set_len_sealed(&self, new_len: u64) -> StarryResult {
        let _guard = self.truncate_mtx.lock();
        let current_len = self.inner.inner().backend()?.location().len()?;
        self.check_truncate(current_len, new_len)?;
        self.inner
            .inner()
            .access(FileFlags::WRITE)?
            .set_len(new_len)?;
        Ok(())
    }

    /// Seal-aware offset write (`pwrite64`/`pwritev2`). Routes around
    /// the underlying `File::write_at` so `F_SEAL_WRITE` rejects with
    /// `EPERM` and `F_SEAL_GROW` is enforced with Linux's
    /// shmem_write_check_limits semantics: a write that straddles EOF
    /// is truncated to the in-EOF bytes (partial success); a write
    /// that starts at or past EOF is rejected with `EPERM`.
    ///
    /// `truncate_mtx` is taken before the seal load so a concurrent
    /// `add_seals(F_SEAL_GROW)` cannot publish between us reading
    /// the seal and performing the write — without that ordering,
    /// the unsealed fast path could escape into a write that grows
    /// the file after the seal landed.
    pub fn write_at(&self, data: &[u8], offset: u64) -> StarryResult<usize> {
        // Zero-length pwrite/pwritev succeeds unconditionally on Linux,
        // even on a sealed memfd, and does not advance the file size.
        // Short-circuit before any seal check (verified against
        // memfd_create + F_ADD_SEALS(F_SEAL_WRITE / F_SEAL_GROW) on a
        // stock host: pwrite(fd, _, 0, _) returns 0 in both cases).
        if data.is_empty() {
            return Ok(0);
        }
        let f = self.inner.inner().access(FileFlags::WRITE)?;
        let _guard = self.truncate_mtx.lock();
        let seals = self.get_seals();
        if seals & F_SEAL_ANY_WRITE != 0 {
            return Err(StarryError::OperationNotPermitted);
        }
        if seals & F_SEAL_GROW == 0 {
            return Ok(f.write_at(data, offset)?);
        }
        // F_SEAL_GROW Linux semantics (verified against memfd_create +
        // F_ADD_SEALS(F_SEAL_GROW) on a stock host):
        //   - cross-EOF write: short-write the bytes that fit before EOF,
        //   - at-EOF or past-EOF write: -1 EPERM.
        // EPERM here is distinct from the F_SEAL_WRITE path above which
        // rejects every write; F_SEAL_GROW only rejects growth.
        let cur_len = self.inner.inner().backend()?.location().len()?;
        if offset >= cur_len {
            return Err(StarryError::OperationNotPermitted);
        }
        let writable = (cur_len - offset).min(data.len() as u64) as usize;
        if writable == 0 {
            return Err(StarryError::OperationNotPermitted);
        }
        Ok(f.write_at(&data[..writable], offset)?)
    }
}

fn memfd_from_file_backend(backend: &MappingOperation) -> Option<Arc<Memfd>> {
    memfd_from_shared_file(&backend.shared_file_lease()?)
}

fn memfd_from_shared_file(file: &SharedFileMappingLease) -> Option<Arc<Memfd>> {
    file.cache_location()
        .user_data()
        .get::<MemfdRef>()
        .map(|memfd| memfd.0.clone())
}

fn memfd_from_shared_writable_area(area: &SharedFileVmaRecord) -> Option<Arc<Memfd>> {
    if !area.rights.contains(MappingFlags::WRITE) {
        return None;
    }
    memfd_from_shared_file(&area.file)
}

fn apply_shared_writable_count_delta(
    memfd: &Memfd,
    mm_id: AddressSpaceId,
    delta: i32,
) {
    let mut mappings = memfd.shared_writable_mmaps.lock();
    if delta > 0 {
        let count = mappings.entry(mm_id).or_default();
        let add = delta as u32;
        let Some(next) = count.checked_add(add) else {
            warn!(
                "memfd shared writable VMA count overflow for mm {} (cur={}, add={add}); retaining saturated busy state",
                mm_id.get(),
                *count
            );
            *count = u32::MAX;
            return;
        };
        *count = next;
    } else if delta < 0 {
        let sub = (-delta) as u32;
        let Some(count) = mappings.get_mut(&mm_id) else {
            warn!(
                "memfd shared writable VMA count missing for mm {} while subtracting {sub}",
                mm_id.get()
            );
            return;
        };
        if *count < sub {
            warn!(
                "memfd shared writable VMA count underflow for mm {} (cur={}, sub={sub}); leaving count unchanged",
                mm_id.get(),
                *count
            );
            return;
        }
        *count -= sub;
        if *count == 0 {
            mappings.remove(&mm_id);
        }
    }
}

/// A side-band update prepared from an address-space mutation.  Preparing the
/// update is read-only; the counter is changed only after the corresponding
/// VMA publication has succeeded.  This keeps memfd seals from observing a
/// mapping that a failed `munmap`/`mremap` left behind.
#[derive(Clone)]
pub(crate) struct SharedWritableDelta {
    memfd: Arc<Memfd>,
    mm_id: AddressSpaceId,
    delta: i32,
}

pub(crate) fn apply_shared_writable_deltas(deltas: &[SharedWritableDelta]) {
    for delta in deltas {
        apply_shared_writable_count_delta(delta.memfd.as_ref(), delta.mm_id, delta.delta);
    }
}

pub fn check_write_seal_for_shared_file_backend(
    file: &SharedFileMappingLease,
) -> StarryResult {
    let Some(memfd) = memfd_from_shared_file(file) else {
        return Ok(());
    };
    memfd.check_write_seal()
}

/// Applies Linux `MADV_REMOVE` to one VMA fragment.
///
/// Linux deliberately walks VMAs in address order and does not roll back an
/// earlier hole when a later VMA fails.  This function therefore owns exactly
/// one file operation: it validates the shared/write-capable file, serializes
/// memfd seals when present, and delegates the mapping/cache transition to the
/// filesystem's `PUNCH_HOLE` operation.  It never emulates a hole with a loop
/// of partial writes.
pub(crate) fn punch_shared_file_backend(
    file_mapping: &SharedFileMappingLease,
    file_offset: u64,
    len: usize,
) -> StarryResult {
    file_mapping.check_flags(MappingFlags::WRITE)?;
    let len = u64::try_from(len).map_err(|_| StarryError::InvalidInput)?;
    file_offset
        .checked_add(len)
        .ok_or(StarryError::InvalidInput)?;
    if len == 0 {
        return Ok(());
    }

    if let Some(memfd) = memfd_from_shared_file(file_mapping) {
        let _seal_guard = memfd.truncate_mtx.lock();
        if memfd.get_seals() & F_SEAL_ANY_WRITE != 0 {
            return Err(StarryError::OperationNotPermitted);
        }
        file_mapping
            .cache()
            .operate_range(file_offset, len, FileRangeOperation::PunchHole)?;
    } else {
        file_mapping
            .cache()
            .operate_range(file_offset, len, FileRangeOperation::PunchHole)?;
    }
    Ok(())
}

pub(crate) fn on_after_map(aspace: &AddrSpace, start: VirtAddr) {
    let Some(area) = aspace.shared_file_vma_at(start) else {
        return;
    };
    let Some(memfd) = memfd_from_shared_writable_area(&area) else {
        return;
    };
    apply_shared_writable_count_delta(memfd.as_ref(), aspace.address_space_id(), 1);
}

/// Computes the memfd writable-count changes caused by removing a range of
/// VMA metadata.  No counter is touched and no callback is invoked.
pub(crate) fn prepare_aspace_unmap_deltas(
    aspace: &AddrSpace,
    ustart: VirtAddr,
    ulen: usize,
) -> Vec<SharedWritableDelta> {
    let Some(uend) = ustart.checked_add(ulen) else {
        return Vec::new();
    };
    let mut deltas = Vec::new();
    for area in aspace.shared_file_vmas() {
        let a0 = area.range.start;
        let a1 = area.range.end;
        if a1 <= ustart || a0 >= uend {
            continue;
        }
        let Some(memfd) = memfd_from_shared_writable_area(&area) else {
            continue;
        };
        if ustart <= a0 && uend >= a1 {
            deltas.push(SharedWritableDelta {
                memfd,
                mm_id: aspace.address_space_id(),
                delta: -1,
            });
        } else if ustart > a0 && uend < a1 {
            // Strict interior unmap splits one writable shared VMA into two.
            deltas.push(SharedWritableDelta {
                memfd,
                mm_id: aspace.address_space_id(),
                delta: 1,
            });
        }
    }
    deltas
}

pub(crate) fn collect_metas_touching_mprotect_range(
    aspace: &AddrSpace,
    ustart: VirtAddr,
    ulen: usize,
) -> Vec<Arc<Memfd>> {
    let Some(uend) = ustart.checked_add(ulen) else {
        return Vec::new();
    };
    let mut memfds = Vec::new();
    for area in aspace.shared_file_vmas() {
        if area.range.end <= ustart || area.range.start >= uend {
            continue;
        }
        let Some(memfd) = memfd_from_shared_file(&area.file) else {
            continue;
        };
        if !memfds.iter().any(|x: &Arc<Memfd>| Arc::ptr_eq(x, &memfd)) {
            memfds.push(memfd);
        }
    }
    memfds
}

pub(crate) fn resync_shared_writable_counts_after_mprotect(
    aspace: &AddrSpace,
    touched: &[Arc<Memfd>],
) {
    for memfd in touched {
        let mut count: u32 = 0;
        for area in aspace.shared_file_vmas() {
            let Some(mapped) = memfd_from_shared_writable_area(&area) else {
                continue;
            };
            if Arc::ptr_eq(&mapped, memfd) {
                count = count.saturating_add(1);
            }
        }
        let mm_id = aspace.address_space_id();
        let mut mappings = memfd.shared_writable_mmaps.lock();
        if count == 0 {
            mappings.remove(&mm_id);
        } else {
            mappings.insert(mm_id, count);
        }
    }
}

/// Computes the old/new writable-count transition for a metadata replacement
/// without publishing it.  The caller owns the commit ordering.
pub(crate) fn prepare_aspace_replace_deltas(
    aspace: &AddrSpace,
    ustart: VirtAddr,
    ulen: usize,
    new_flags: MappingFlags,
    new_backend: &MappingOperation,
) -> Vec<SharedWritableDelta> {
    let mut deltas = Vec::new();
    let Some(uend) = ustart.checked_add(ulen) else {
        return deltas;
    };
    for old in aspace.shared_file_vmas() {
        if old.range.end <= ustart || old.range.start >= uend {
            continue;
        }
        if let Some(memfd) = memfd_from_shared_file(&old.file)
            && old.rights.contains(MappingFlags::WRITE)
        {
            deltas.push(SharedWritableDelta {
                memfd,
                mm_id: aspace.address_space_id(),
                delta: -1,
            });
        }
    }
    if let Some(memfd) = memfd_from_file_backend(new_backend)
        && new_flags.contains(MappingFlags::WRITE)
    {
        deltas.push(SharedWritableDelta {
            memfd,
            mm_id: aspace.address_space_id(),
            delta: 1,
        });
    }
    deltas
}

impl FileLike for Memfd {
    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        self.inner.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        // Zero-length write(2)/writev(2) (including pwritev2 with an
        // empty iov, sys_splice's zero-byte output probe, and similar)
        // succeeds unconditionally on Linux even against a sealed memfd
        // and never advances the file. Short-circuit before any seal
        // check so a count==0 write returns 0 rather than synthesizing
        // EPERM. Verified on a stock host against F_SEAL_WRITE and
        // F_SEAL_GROW.
        if src.remaining() == 0 {
            return Ok(0);
        }
        // Hold `truncate_mtx` across the seal read and the write so a
        // concurrent `add_seals(F_SEAL_GROW)` cannot publish in between
        // and let an unsealed write grow the file after the seal was
        // supposed to land. `add_seals` also takes this lock when
        // publishing.
        let _guard = self.truncate_mtx.lock();
        let seals = self.get_seals();
        if seals & F_SEAL_ANY_WRITE != 0 {
            return Err(StarryError::OperationNotPermitted);
        }
        if seals & F_SEAL_GROW == 0 {
            return self.inner.write(src);
        }
        // F_SEAL_GROW Linux semantics, verified against
        // memfd_create + F_ADD_SEALS(F_SEAL_GROW) on a stock host:
        //   - cross-EOF write/writev short-writes the bytes that fit
        //     before EOF and reports that partial count.
        //   - write starting at or past EOF returns -1 EPERM and
        //     leaves the file untouched.
        // The previous "write-then-rollback" approach modified in-EOF
        // bytes before reporting failure and lost the partial-write
        // semantics. Drain only the in-range bytes into a buffer and
        // route them through `write_at` at the current cursor; then
        // advance the inner cursor manually so the next sequential
        // write picks up correctly.
        let cur_len = self.inner.inner().backend()?.location().len()?;
        let cursor = self.inner.inner().seek(SeekFrom::Current(0))?;
        if cursor >= cur_len {
            return Err(StarryError::OperationNotPermitted);
        }
        let max_writable = (cur_len - cursor) as usize;
        let want = src.remaining().min(max_writable);
        if want == 0 {
            return Ok(0);
        }
        let f = self.inner.inner().access(FileFlags::WRITE)?;
        let mut buf = alloc::vec![0u8; want];
        let n = src.read(&mut buf)?;
        if n == 0 {
            return Ok(0);
        }
        let written = f.write_at(&buf[..n], cursor)?;
        if written > 0 {
            // Advance the inner cursor to match the sequential
            // semantics expected by the caller (write(2) leaves the
            // cursor positioned after the last byte written).
            let _ = self.inner.inner().seek(SeekFrom::Current(written as i64));
        }
        Ok(written)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        self.inner.stat()
    }

    fn path(&self) -> Cow<'_, str> {
        // Linux reports memfds as `/memfd:<name> (deleted)` via
        // `readlink /proc/<pid>/fd/<n>`. We drop the " (deleted)" suffix
        // since callers here are primarily internal `path()` consumers
        // not readlink.
        format!("/memfd:{}", self.name).into()
    }

    fn file_mmap(&self) -> StarryResult<(ax_fs_ng::vfs::FileBackend, ax_fs_ng::vfs::FileFlags)> {
        // Reuse the inner File's mmap path so file-backed shared/private
        // mappings on memfd fds work the same as on regular files. Seal
        // enforcement for `MAP_SHARED|PROT_WRITE` runs in `sys_mmap`
        // before this is called.
        self.inner.file_mmap()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> StarryResult<usize> {
        self.inner.ioctl(cmd, arg)
    }

    fn open_flags(&self) -> u32 {
        self.inner.open_flags()
    }

    fn nonblocking(&self) -> bool {
        self.inner.nonblocking()
    }

    fn set_nonblocking(&self, non_blocking: bool) -> StarryResult {
        self.inner.set_nonblocking(non_blocking)
    }

    fn from_fd(fd: core::ffi::c_int) -> StarryResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        let any = get_file_like(fd)?;
        if let Ok(memfd) = any.clone().downcast_arc::<Memfd>() {
            return Ok(memfd);
        }
        let Some(file) = any.downcast_ref::<File>() else {
            return Err(StarryError::InvalidInput);
        };
        file.inner()
            .backend()?
            .location()
            .user_data()
            .get::<MemfdRef>()
            .map(|memfd| memfd.0.clone())
            .ok_or(StarryError::InvalidInput)
    }
}

impl Pollable for Memfd {
    fn poll(&self) -> IoEvents {
        self.inner.poll()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.inner.register(context, events);
    }
}
