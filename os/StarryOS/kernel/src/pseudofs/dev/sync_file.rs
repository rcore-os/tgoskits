//! Minimal Linux `sync_file` fd object (UAPI alignment of
//! `drivers/dma-buf/sync_file.c`).
//!
//! The only consumer is card0's `VIRTGPU_EXECBUFFER` `FENCE_FD_OUT` path. The
//! current virtio-gpu transport completes a fenced `SUBMIT_3D` synchronously:
//! `submit_cmd()` returns only once the host fence response has been popped
//! from the virtqueue. The out-fence is therefore already satisfied when the
//! ioctl returns, so this object is signaled *before* its fd becomes reachable
//! through the file table. It reports readiness as a pure level instead of
//! forging a pending completion.
//!
//! UAPI reference (`include/uapi/linux/sync_file.h`):
//! - `SYNC_IOC_FILE_INFO` = `_IOWR('>', 4, struct sync_file_info)`; `status`
//!   is 1 signaled / 0 active, `num_fences == 0` publishes the fence count,
//!   and a non-null `sync_fence_info` buffer receives one entry.
//! - `SYNC_IOC_MERGE` / `SYNC_IOC_SET_DEADLINE` and the legacy `SYNC_IOC_WAIT`
//!   have no consumer here and keep the generic `ENOTTY`.
//! - `poll`/`epoll` report `POLLIN` once signaled.

use alloc::borrow::Cow;
use core::sync::atomic::{AtomicBool, Ordering};

use axpoll::{IoEvents, Pollable};
use axpoll_set::PollSet;
use bytemuck::{AnyBitPattern, NoUninit};

use crate::{
    StarryError, StarryResult,
    file::FileLike,
    mm::{VmMutPtr, VmPtr},
    task::UserTaskRef,
};

/// Linux `_IOC` direction bits (`include/uapi/asm-generic/ioctl.h`).
const IOC_READ: u32 = 2;
const IOC_WRITE: u32 = 1;

/// Packs a Linux ioctl request: `dir | size | type | nr` (bit 31..30 | 29..16 |
/// 15..8 | 7..0).
const fn ioc(dir: u32, ty: u8, nr: u8, size: u16) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | (nr as u32)
}

/// `SYNC_IOC_FILE_INFO = _IOWR('>', 4, struct sync_file_info)` — encodes to
/// `0xc0383e04` (type `>` = 0x3e, nr 4, size 56). Matching the full request
/// keeps a wrong direction or struct size at the generic `ENOTTY`, exactly as
/// Linux's ioctl table does.
const SYNC_IOC_FILE_INFO: u32 = ioc(
    IOC_READ | IOC_WRITE,
    b'>',
    4,
    core::mem::size_of::<SyncFileInfo>() as u16,
);

/// Timeline name reported through `SYNC_IOC_FILE_INFO` (`sync_fence_info`).
const FENCE_NAME: &str = "starry-fence";
/// Driver name reported through `SYNC_IOC_FILE_INFO`.
const FENCE_DRIVER_NAME: &str = "starry-card0";

/// `struct sync_file_info` — `SYNC_IOC_FILE_INFO` payload (56 bytes on 64-bit).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct SyncFileInfo {
    /// Output: fence name, zero-padded to 32 bytes.
    pub name: [u8; 32],
    /// Output: 1 signaled / 0 active.
    pub status: i32,
    pub flags: u32,
    /// In/out: in = capacity of `sync_fence_info`, out = actual fence count.
    pub num_fences: u32,
    pub pad: u32,
    /// User pointer to an array of [`SyncFenceInfo`].
    pub sync_fence_info: u64,
}

/// `struct sync_fence_info` — per-fence detail entry (80 bytes on 64-bit).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, AnyBitPattern, NoUninit)]
pub struct SyncFenceInfo {
    pub obj_name: [u8; 32],
    pub driver_name: [u8; 32],
    /// 1 signaled / 0 active / negative error.
    pub status: i32,
    pub flags: u32,
    pub timestamp_ns: u64,
}

/// A `sync_file` backed by one already-completed GPU submit fence.
///
/// The signaled bit is published before the fd is installed, so a woken or
/// registering poller always observes the completion it was woken for. The
/// [`PollSet`] only exists so an `epoll` interest registered on a signaled fd
/// stays coherent; no producer signals this object after it becomes reachable.
pub struct SyncFile {
    signaled: AtomicBool,
    poll_rx: PollSet,
}

impl SyncFile {
    /// Creates an unsignaled sync_file. Callers must publish completion with
    /// [`Self::mark_signaled`] before installing the fd.
    pub fn new() -> Self {
        Self {
            signaled: AtomicBool::new(false),
            poll_rx: PollSet::new(),
        }
    }

    /// Publishes the signaled state and wakes any poll/epoll sleepers.
    ///
    /// Task context only: [`PollSet::wake`] must not run in hard IRQ. The
    /// readiness flag is published by the `swap` before any woken thread
    /// reloads it.
    pub fn mark_signaled(&self) {
        if !self.signaled.swap(true, Ordering::Release) {
            // SAFETY: task context; readiness was published by the `swap`
            // above before any woken thread reloads it.
            unsafe { self.poll_rx.wake(IoEvents::IN) };
        }
    }

    /// Returns whether the fence has been signaled.
    pub fn is_signaled(&self) -> bool {
        self.signaled.load(Ordering::Acquire)
    }

    /// The fence status as reported by `SYNC_IOC_FILE_INFO` (1/0).
    fn status(&self) -> i32 {
        if self.is_signaled() { 1 } else { 0 }
    }
}

impl FileLike for SyncFile {
    fn validate_write_access(&self) -> StarryResult {
        Err(StarryError::InvalidInput)
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:sync_file".into()
    }

    fn ioctl(&self, current: &UserTaskRef, cmd: u32, arg: usize) -> StarryResult<usize> {
        match cmd {
            SYNC_IOC_FILE_INFO => self.ioctl_file_info(current, arg),
            _ => Err(StarryError::NotATty),
        }
    }
}

impl SyncFile {
    /// `SYNC_IOC_FILE_INFO` — mirror of `sync_file_ioctl_fence_info`.
    fn ioctl_file_info(&self, current: &UserTaskRef, arg: usize) -> StarryResult<usize> {
        let ptr = arg as *mut SyncFileInfo;
        let mut info: SyncFileInfo = ptr.vm_read(current).map_err(|_| StarryError::BadAddress)?;

        if info.flags != 0 || info.pad != 0 {
            return Err(StarryError::InvalidInput);
        }

        let status = self.status();
        // One fence per submit; a count-only query still reports it.
        let num_fences = 1u32;
        if info.num_fences != 0 {
            if info.num_fences < num_fences {
                return Err(StarryError::InvalidInput);
            }
            if info.sync_fence_info == 0 {
                return Err(StarryError::BadAddress);
            }
            let entry = SyncFenceInfo {
                obj_name: name_bytes(FENCE_NAME),
                driver_name: name_bytes(FENCE_DRIVER_NAME),
                status,
                flags: 0,
                timestamp_ns: 0,
            };
            (info.sync_fence_info as *mut SyncFenceInfo)
                .vm_write(current, entry)
                .map_err(|_| StarryError::BadAddress)?;
        }

        info.name = name_bytes(FENCE_NAME);
        info.status = status;
        info.num_fences = num_fences;
        ptr.vm_write(current, info)
            .map_err(|_| StarryError::BadAddress)?;
        Ok(0)
    }
}

/// Zero-padded fixed-size name, mirroring `strscpy` into a `char[32]`.
fn name_bytes(name: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let bytes = name.as_bytes();
    let len = bytes.len().min(out.len() - 1);
    out[..len].copy_from_slice(&bytes[..len]);
    out
}

impl Pollable for SyncFile {
    fn poll(&self) -> IoEvents {
        if self.is_signaled() {
            IoEvents::IN
        } else {
            IoEvents::empty()
        }
    }

    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        if events.contains(IoEvents::IN) {
            unsafe { sink.register_shared(&self.poll_rx, IoEvents::IN) };
        }
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        if events.contains(IoEvents::IN) {
            unsafe { sink.register_exclusive(&self.poll_rx, IoEvents::IN) };
        }
    }
}
