//! `memfd_create` backing object.
//!
//! A `Memfd` wraps a regular tmpfs-backed `File` and adds a per-fd seal mask
//! so userspace can call `fcntl(F_ADD_SEALS, F_GET_SEALS)` and see Linux
//! semantics. The backing file lives under `/tmp/` with a name derived from
//! the creator's pid plus a per-process counter, and is unlinked at create
//! time so `fstat(fd).st_nlink == 0` matches Linux's anonymous-inode model.
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

use alloc::{borrow::Cow, format, string::String, sync::Arc};
use core::{
    sync::atomic::{AtomicU32, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use ax_fs::FileFlags;
use ax_sync::Mutex;
use axpoll::{IoEvents, Pollable};

use super::{File, FileLike, IoDst, IoSrc, Kstat};

pub const F_SEAL_SEAL: u32 = 0x0001;
pub const F_SEAL_SHRINK: u32 = 0x0002;
pub const F_SEAL_GROW: u32 = 0x0004;
pub const F_SEAL_WRITE: u32 = 0x0008;

/// Mask of bits that can ever appear in a seal mask.
pub const F_SEAL_ALL: u32 = F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE;

pub struct Memfd {
    inner: Arc<File>,
    seals: AtomicU32,
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
            name,
            truncate_mtx: Mutex::new(()),
        })
    }

    pub fn inner(&self) -> &Arc<File> {
        &self.inner
    }

    pub fn get_seals(&self) -> u32 {
        self.seals.load(Ordering::Acquire)
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
        let current_len = self.inner.inner().backend()?.location().len()?;
        self.check_truncate(current_len, new_len)?;
        self.inner
            .inner()
            .access(FileFlags::WRITE)?
            .set_len(new_len)?;
        Ok(())
    }
}

impl FileLike for Memfd {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        self.inner.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        let seals = self.get_seals();
        if seals & F_SEAL_WRITE != 0 {
            return Err(AxError::OperationNotPermitted);
        }
        if seals & F_SEAL_GROW == 0 {
            return self.inner.write(src);
        }
        // F_SEAL_GROW: writes past current EOF must fail. The inner
        // File owns the cursor privately, so we can't pre-check
        // "would-extend" by computing `pos + len > size`; instead we
        // serialize against ftruncate and other sealed writes (so the
        // size we observe stays stable), perform the write, and snap
        // back to the pre-write size if it grew. Observable contract
        // matches Linux's shmem_write_check_limits: file size never
        // grows under the seal, and the call reports EPERM.
        let _guard = self.truncate_mtx.lock();
        let pre_len = self.inner.inner().backend()?.location().len()?;
        let written = self.inner.write(src)?;
        let post_len = self.inner.inner().backend()?.location().len()?;
        if post_len > pre_len {
            if let Ok(f) = self.inner.inner().access(FileFlags::WRITE) {
                let _ = f.set_len(pre_len);
            }
            return Err(AxError::OperationNotPermitted);
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

    fn file_mmap(&self) -> AxResult<(ax_fs::FileBackend, ax_fs::FileFlags)> {
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
}

impl Pollable for Memfd {
    fn poll(&self) -> IoEvents {
        self.inner.poll()
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        self.inner.register(context, events);
    }
}
