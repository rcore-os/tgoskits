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
//!     `MAP_SHARED|PROT_WRITE` mmap calls
//!
//! Remaining gap vs. Linux (will be addressed in a follow-up PR):
//!   - `F_SEAL_WRITE` does not revoke write access on extant
//!     `MAP_SHARED|PROT_WRITE` mappings — the seal only blocks new mmap
//!     calls. Implementing live-mapping revocation needs a registry of
//!     installed VMAs and a way to call `aspace.protect` from the seal
//!     path; deferred to keep this PR focused on the seal mask itself.
//!
//! Wayland's `wl_shm` requires `F_SEAL_SHRINK`, which is fully enforced.

use alloc::{borrow::Cow, format, string::String, sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicU32, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use ax_fs_ng::vfs::FileFlags;
use ax_io::{IoBuf, SeekFrom, prelude::*};
use ax_memory_addr::VirtAddr;
use ax_memory_set::MemoryArea;
use ax_runtime::hal::paging::MappingFlags;
use ax_sync::PiMutex;
use axpoll::{IoEvents, Pollable};

use super::{File, FileLike, IoDst, IoSrc, Kstat, get_file_like};
use crate::mm::{AddrSpace, Backend};

pub const F_SEAL_SEAL: u32 = 0x0001;
pub const F_SEAL_SHRINK: u32 = 0x0002;
pub const F_SEAL_GROW: u32 = 0x0004;
pub const F_SEAL_WRITE: u32 = 0x0008;

/// Mask of bits that can ever appear in a seal mask.
pub const F_SEAL_ALL: u32 = F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE;

#[derive(Clone)]
pub struct MemfdRef(pub Arc<Memfd>);

pub struct Memfd {
    inner: Arc<File>,
    seals: AtomicU32,
    /// Number of live MAP_SHARED writable VMAs for this memfd.
    shared_writable_mmap_count: AtomicU32,
    /// Userspace-visible name (the `name` arg to `memfd_create`). Included
    /// in the reported path so `/proc/*/fd/*` matches Linux's
    /// `/memfd:<name>` convention.
    name: String,
    /// Serializes seal-check-and-truncate to close the TOCTOU window
    /// between `check_truncate` and the underlying `set_len`.
    truncate_mtx: PiMutex<()>,
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
            shared_writable_mmap_count: AtomicU32::new(0),
            name,
            truncate_mtx: PiMutex::new(()),
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

    pub fn check_write_seal(&self) -> AxResult {
        if self.get_seals() & F_SEAL_WRITE != 0 {
            Err(AxError::OperationNotPermitted)
        } else {
            Ok(())
        }
    }

    /// Add the given seals to the current set. Returns `OperationNotPermitted`
    /// if `F_SEAL_SEAL` is already set (so the mask is frozen), or
    /// `InvalidInput` if the requested seal bits are outside the supported
    /// mask.
    pub fn add_seals(&self, add: u32) -> AxResult {
        if add & !F_SEAL_ALL != 0 {
            return Err(AxError::InvalidInput);
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
                return Err(AxError::OperationNotPermitted);
            }
            if add & F_SEAL_WRITE != 0
                && prev & F_SEAL_WRITE == 0
                && self.shared_writable_mmap_count.load(Ordering::SeqCst) > 0
            {
                return Err(AxError::ResourceBusy);
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
    fn check_truncate(&self, current_len: u64, new_len: u64) -> AxResult {
        let seals = self.get_seals();
        if new_len < current_len && seals & F_SEAL_SHRINK != 0 {
            return Err(AxError::OperationNotPermitted);
        }
        if new_len > current_len && seals & F_SEAL_GROW != 0 {
            return Err(AxError::OperationNotPermitted);
        }
        Ok(())
    }

    /// Seal-aware `ftruncate`. Holds `truncate_mtx` across the length
    /// query, seal check, and underlying `set_len` to close the TOCTOU
    /// window: without this lock, two concurrent `ftruncate` calls could
    /// both read the pre-shrink size, both pass `check_truncate`, and
    /// both race on `set_len`, with only the last write observed.
    pub fn set_len_sealed(&self, new_len: u64) -> AxResult {
        let _guard = self.truncate_mtx.lock();
        let current_len = self.inner.inner().backend()?.len()?;
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
    pub fn write_at(&self, data: &[u8], offset: u64) -> AxResult<usize> {
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
        if seals & F_SEAL_WRITE != 0 {
            return Err(AxError::OperationNotPermitted);
        }
        if seals & F_SEAL_GROW == 0 {
            return f.write_at(data, offset);
        }
        // F_SEAL_GROW Linux semantics (verified against memfd_create +
        // F_ADD_SEALS(F_SEAL_GROW) on a stock host):
        //   - cross-EOF write: short-write the bytes that fit before EOF,
        //   - at-EOF or past-EOF write: -1 EPERM.
        // EPERM here is distinct from the F_SEAL_WRITE path above which
        // rejects every write; F_SEAL_GROW only rejects growth.
        let cur_len = self.inner.inner().backend()?.len()?;
        if offset >= cur_len {
            return Err(AxError::OperationNotPermitted);
        }
        let writable = (cur_len - offset).min(data.len() as u64) as usize;
        if writable == 0 {
            return Err(AxError::OperationNotPermitted);
        }
        f.write_at(&data[..writable], offset)
    }
}

fn memfd_from_file_backend(backend: &Backend) -> Option<Arc<Memfd>> {
    let Backend::File(f) = backend else {
        return None;
    };
    if !f.is_shared_file_map() {
        return None;
    }
    f.cache_user_data::<MemfdRef>()
        .ok()
        .flatten()
        .map(|memfd| memfd.0.clone())
}

fn memfd_from_shared_writable_area(area: &MemoryArea<Backend>) -> Option<Arc<Memfd>> {
    if !area.flags().contains(MappingFlags::WRITE) {
        return None;
    }
    memfd_from_file_backend(area.backend())
}

fn apply_shared_writable_count_delta(memfd: &Memfd, delta: i32) {
    if delta > 0 {
        memfd
            .shared_writable_mmap_count
            .fetch_add(delta as u32, Ordering::SeqCst);
    } else if delta < 0 {
        let sub = (-delta) as u32;
        loop {
            let cur = memfd.shared_writable_mmap_count.load(Ordering::SeqCst);
            if cur < sub {
                warn!(
                    "memfd shared_writable_mmap_count underflow (cur={cur}, sub={sub}); leaving \
                     counter unchanged"
                );
                debug_assert!(
                    cur >= sub,
                    "memfd shared_writable_mmap_count underflow (cur={cur}, sub={sub})"
                );
                break;
            }
            let next = cur - sub;
            if memfd
                .shared_writable_mmap_count
                .compare_exchange_weak(cur, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }
    }
}

pub fn check_write_seal_for_shared_file_backend(backend: &Backend) -> AxResult {
    let Some(memfd) = memfd_from_file_backend(backend) else {
        return Ok(());
    };
    memfd.check_write_seal()
}

pub(crate) fn apply_shared_writable_delta_for_backend(
    backend: &Backend,
    old_flags: MappingFlags,
    new_flags: MappingFlags,
) {
    let Some(memfd) = memfd_from_file_backend(backend) else {
        return;
    };
    let old_writable = old_flags.contains(MappingFlags::WRITE);
    let new_writable = new_flags.contains(MappingFlags::WRITE);
    apply_shared_writable_count_delta(memfd.as_ref(), new_writable as i32 - old_writable as i32);
}

pub(crate) fn on_after_map(aspace: &AddrSpace, start: VirtAddr) {
    let Some(area) = aspace.find_area(start) else {
        return;
    };
    let Some(memfd) = memfd_from_shared_writable_area(area) else {
        return;
    };
    apply_shared_writable_count_delta(memfd.as_ref(), 1);
}

pub(crate) fn on_aspace_unmap_range(aspace: &AddrSpace, ustart: VirtAddr, ulen: usize) {
    let uend = ustart + ulen;
    for area in aspace.areas() {
        let a0 = area.start();
        let a1 = area.end();
        if a1 <= ustart || a0 >= uend {
            continue;
        }
        let Some(memfd) = memfd_from_shared_writable_area(area) else {
            continue;
        };
        if ustart <= a0 && uend >= a1 {
            apply_shared_writable_count_delta(memfd.as_ref(), -1);
        } else if ustart > a0 && uend < a1 {
            // Strict interior unmap splits one writable shared VMA into two.
            apply_shared_writable_count_delta(memfd.as_ref(), 1);
        }
    }
}

pub(crate) fn collect_metas_touching_mprotect_range(
    aspace: &AddrSpace,
    ustart: VirtAddr,
    ulen: usize,
) -> Vec<Arc<Memfd>> {
    let uend = ustart + ulen;
    let mut memfds = Vec::new();
    for area in aspace.areas() {
        if area.end() <= ustart || area.start() >= uend {
            continue;
        }
        let Some(memfd) = memfd_from_file_backend(area.backend()) else {
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
        for area in aspace.areas() {
            let Some(mapped) = memfd_from_shared_writable_area(area) else {
                continue;
            };
            if Arc::ptr_eq(&mapped, memfd) {
                count = count.saturating_add(1);
            }
        }
        memfd
            .shared_writable_mmap_count
            .store(count, Ordering::SeqCst);
    }
}

pub(crate) fn release_all_shared_writable_counts_for_aspace(aspace: &AddrSpace) {
    for area in aspace.areas() {
        let Some(memfd) = memfd_from_shared_writable_area(area) else {
            continue;
        };
        apply_shared_writable_count_delta(memfd.as_ref(), -1);
    }
}

pub(crate) fn on_aspace_replace_metadata(
    aspace: &AddrSpace,
    ustart: VirtAddr,
    ulen: usize,
    new_flags: MappingFlags,
    new_backend: &Backend,
) {
    let empty = MappingFlags::empty();
    for (_frag_start, _frag_size, old_flags, old_backend) in aspace.areas_in_range(ustart, ulen) {
        apply_shared_writable_delta_for_backend(&old_backend, old_flags, empty);
    }
    apply_shared_writable_delta_for_backend(new_backend, empty, new_flags);
}

impl FileLike for Memfd {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        self.inner.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
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
        if seals & F_SEAL_WRITE != 0 {
            return Err(AxError::OperationNotPermitted);
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
        let cur_len = self.inner.inner().backend()?.len()?;
        let cursor = self.inner.inner().seek(SeekFrom::Current(0))?;
        if cursor >= cur_len {
            return Err(AxError::OperationNotPermitted);
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

    fn stat(&self) -> AxResult<Kstat> {
        self.inner.stat()
    }

    fn path(&self) -> Cow<'_, str> {
        // Linux reports memfds as `/memfd:<name> (deleted)` via
        // `readlink /proc/<pid>/fd/<n>`. We drop the " (deleted)" suffix
        // since callers here are primarily internal `path()` consumers
        // not readlink.
        format!("/memfd:{}", self.name).into()
    }

    fn file_mmap(&self) -> AxResult<(ax_fs_ng::vfs::FileBackend, ax_fs_ng::vfs::FileFlags)> {
        // Reuse the inner File's mmap path so file-backed shared/private
        // mappings on memfd fds work the same as on regular files. Seal
        // enforcement for `MAP_SHARED|PROT_WRITE` runs in `sys_mmap`
        // before this is called.
        self.inner.file_mmap()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        self.inner.ioctl(cmd, arg)
    }

    fn open_flags(&self) -> u32 {
        self.inner.open_flags()
    }

    fn nonblocking(&self) -> bool {
        self.inner.nonblocking()
    }

    fn set_nonblocking(&self, non_blocking: bool) -> AxResult {
        self.inner.set_nonblocking(non_blocking)
    }

    fn from_fd(fd: core::ffi::c_int) -> AxResult<Arc<Self>>
    where
        Self: Sized + 'static,
    {
        let any = get_file_like(fd)?;
        if let Ok(memfd) = any.clone().downcast_arc::<Memfd>() {
            return Ok(memfd);
        }
        let Some(file) = any.downcast_ref::<File>() else {
            return Err(AxError::InvalidInput);
        };
        file.inner()
            .backend()?
            .get_user_data::<MemfdRef>()?
            .map(|memfd| memfd.0.clone())
            .ok_or(AxError::InvalidInput)
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
