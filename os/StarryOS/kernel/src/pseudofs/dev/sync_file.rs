//! Minimal Linux `sync_file` fd object (UAPI alignment of
//! `drivers/dma-buf/sync_file.c`).
//!
//! The only consumer is card0's EXECBUFFER fence path. Guest submits are
//! fire-and-forget (`submit` enqueues and returns a fence token; the fence
//! flag rides on the command), so an out-fence is a *real* fence: it starts
//! unsignaled and flips once the host completed the submit (the used-ring pop
//! advances the device's completion level). This matches Linux
//! `VIRTGPU_EXECBUF_FENCE_FD_OUT` (`virtgpu_ioctl.c`): the kernel wraps the
//! dma-fence in a sync_file and `sync_file_poll` reports POLLIN when the
//! fence fires.
//!
//! UAPI reference (`include/uapi/linux/sync_file.h`, Linux master; opcodes
//! 0-2 were burned by the sync-framework v1→v2 revert and `sync_file_ioctl`
//! answers them with the generic `-ENOTTY`):
//! - `SYNC_IOC_MERGE` = `_IOWR('>', 3, struct sync_merge_data)`;
//!   `SYNC_IOC_FILE_INFO` = `_IOWR('>', 4, struct sync_file_info)`;
//!   `SYNC_IOC_SET_DEADLINE` = `_IOW('>', 5, struct sync_set_deadline_data)`.
//!   Only `SYNC_IOC_FILE_INFO` has a consumer here; `MERGE`/`SET_DEADLINE`
//!   keep the generic `ENOTTY`.
//! - Waiting happens exactly like mainline: userspace polls the fd and
//!   `sync_file_poll` reports `POLLIN` once signaled. There is no WAIT
//!   opcode in mainline to mirror.
//! - `SYNC_IOC_FILE_INFO`: `status` is 1 signaled / 0 active, `num_fences
//!   == 0` publishes the fence count, a non-null `sync_fence_info` buffer
//!   receives one entry.
//! - `poll`/`epoll` report `POLLIN` once signaled.
//!
//! Wakeups: a [`PollSet`] drives poll/epoll sleepers. Completion is observed
//! two ways: waiter-driven refresh (each poll level check queries the fence
//! level, pumping the used ring as a side effect) and a background refresher
//! task, which exists for a guest blocked in `poll()`: a sleeping poll cannot
//! re-check the level by itself. The device's completion IRQ advances the
//! level through the GPU IRQ worker as well; the refresher is the tick-bounded
//! fallback when no waiter is driving the query itself.

use alloc::{
    borrow::Cow,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_runtime::hal::time::monotonic_time_nanos;
use ax_std::os::arceos::task::sync::WaitQueue;
use axpoll::{IoEvents, Pollable};
use axpoll_set::PollSet;
use bytemuck::{AnyBitPattern, NoUninit};

use crate::{
    StarryError, StarryResult,
    file::FileLike,
    mm::{VmMutPtr, VmPtr},
    sync::IrqMutex,
    task::{UserTaskRef, kernel_thread_builder, sleep},
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

/// A sync_file backed by one GPU submit fence.
///
/// The signaled bit is published (`Release`) before pollers are woken and
/// read with `Acquire`, so a woken waiter always observes the completion it
/// was woken for.
pub struct SyncFile {
    /// The `submit_3d` fence id whose host completion signals this file.
    /// Written once by [`Self::bind_fence_id`] before the fd becomes visible
    /// to any other thread, so relaxed accesses suffice.
    fence_id: AtomicU64,
    signaled: AtomicBool,
    poll_rx: PollSet,
}

/// Live out-fence registry. The host completion of a fence is only observable
/// as a level (the device's completion high-water mark); a guest *blocked in
/// poll()* cannot re-check that level by itself, so a background refresher
/// task pumps the used ring and wakes matching pollers. Entries are `Weak`;
/// dead ones are pruned by the same scan. The lock is an IRQ-save mutex so the
/// IRQ path can never spin on it.
static FENCE_WAITERS: IrqMutex<Vec<(u64, Weak<SyncFile>)>> = IrqMutex::new(Vec::new());

/// One-shot guard so the refresher task is spawned exactly once (on the first
/// registered out-fence).
static REFRESHER_SPAWNED: AtomicBool = AtomicBool::new(false);
/// Active service tick while any live out-fence exists: refresh + wake on a
/// 250 µs cadence. The guest's per-frame fence throttle (Mesa waits the
/// previous submit's out-fence around swap) makes this delay show up directly
/// in the frame time: every sleeping-poll wakeup is bounded by one tick, and
/// at ~2000 fps a 1 ms tick cost the whole difference against the blocking
/// baseline (measured: all light glmark2 scenes clamped at ~1700 fps). The
/// device's completion IRQ is the precise fast path (it wakes the GPU IRQ
/// worker, which pumps and lets the next tick observe the level); the short
/// tick bounds the damage while touching nothing else. All non-poll wait
/// paths (WAIT ioctl, in-fence waits) re-check the level themselves and do
/// not depend on this cadence.
const REFRESHER_ACTIVE_TICK: Duration = Duration::from_micros(250);

/// Idle backstop tick while no live out-fence exists: only prunes dead
/// registry entries. 20 wakes/s do not contend for the scheduler.
const REFRESHER_IDLE_TICK: Duration = Duration::from_millis(50);

/// Burst window after an execbuffer kick: the refresher re-pumps the used
/// ring at 50 µs cadence so a just-submitted fence signals within ~one host
/// round-trip instead of waiting for the next active tick. Fallback path for
/// when the completion IRQ is unavailable.
static REFRESHER_BURST_UNTIL_NS: AtomicU64 = AtomicU64::new(0);
const REFRESHER_BURST_WINDOW_NS: u64 = 500_000;
const REFRESHER_BURST_TICK: Duration = Duration::from_micros(50);

/// Parks the backstop refresher while fences are being waited on; an
/// execbuffer burst kick (`kick_refresher`) wakes it immediately.
static REFRESHER_WAKE: WaitQueue = WaitQueue::new();

/// Queries the GPU completion level for `fence_id` through the registered
/// device's virgl extension.
///
/// A lost device ends every outstanding completion (Linux signals all fences
/// on device removal): it is reported as signaled so pollers do not wait
/// forever on a device that can never complete them.
fn fence_level(fence_id: u64) -> bool {
    match ax_gpu::with_gpu(|device| {
        device
            .virgl()
            .ok_or(ax_gpu::rdif_gpu::GpuError::Unsupported)
            .and_then(|virgl| virgl.fence_completed(fence_id))
    }) {
        Ok(Ok(done)) => done,
        Ok(Err(ax_gpu::rdif_gpu::GpuError::DeviceLost))
        | Err(ax_gpu::rdif_gpu::GpuError::DeviceLost) => true,
        // No GPU or virgl extension behind the main device: nothing can
        // complete this fence; keep reporting the unsignaled level.
        _ => false,
    }
}

impl SyncFile {
    /// Creates an unsignaled sync_file for `fence_id`. Call
    /// [`Self::register`] right after the `Arc` is created.
    pub fn new(fence_id: u64) -> Self {
        Self {
            fence_id: AtomicU64::new(fence_id),
            signaled: AtomicBool::new(false),
            poll_rx: PollSet::new(),
        }
    }

    /// Binds the submit's fence id to a file created before the submit.
    ///
    /// EXECBUFFER reserves the out-fence fd before any host-side effect so
    /// an fd shortage fails with no queued GPU work (Linux reserves the fd
    /// with `get_unused_fd_flags` before `virtio_gpu_execbuffer`), but the
    /// fence id only exists once the submit returned. Called exactly once,
    /// after the submit and before [`Self::register`] or the fd install
    /// publishes the file to any other thread.
    pub fn bind_fence_id(&self, fence_id: u64) {
        self.fence_id.store(fence_id, Ordering::Relaxed);
    }

    /// Registers this out-fence in the completion registry and makes sure the
    /// refresher task exists (card0's EXECBUFFER `FENCE_FD_OUT` path).
    ///
    /// `lock` is IRQ-save on both sides (here and in the scans), so the
    /// completion IRQ can never spin on the registry while a task holds it.
    pub fn register(self: &Arc<Self>) {
        FENCE_WAITERS
            .lock()
            .push((self.fence_id.load(Ordering::Relaxed), Arc::downgrade(self)));
        ensure_refresher();
    }

    /// Publishes the signaled state and wakes any poll/epoll sleepers.
    ///
    /// Task context only: [`PollSet::wake`] must not run in hard IRQ. The
    /// readiness flag is published by the `swap` before any woken thread
    /// reloads it.
    fn mark_signaled(&self) {
        if !self.signaled.swap(true, Ordering::Release) {
            // SAFETY: task context; readiness was published by the `swap`
            // above before any woken thread reloads it.
            unsafe { self.poll_rx.wake(IoEvents::IN) };
        }
    }

    /// Polls the underlying GPU fence and returns the current signaled state.
    ///
    /// The fence query delivers the accumulated batch and drains the used
    /// ring as a side effect (`fence_completed` pumps), so waiter-driven
    /// refresh alone advances the completion level even without an IRQ.
    pub fn refresh(&self) -> bool {
        if !self.signaled.load(Ordering::Acquire)
            && fence_level(self.fence_id.load(Ordering::Relaxed))
        {
            self.mark_signaled();
        }
        self.signaled.load(Ordering::Acquire)
    }

}

impl Drop for SyncFile {
    fn drop(&mut self) {
        // Fence ids are unique among live out-fences (one SyncFile per
        // submit), so removing every entry with this id is exact. The IRQ-save
        // lock discipline matches `register`.
        FENCE_WAITERS
            .lock()
            .retain(|(id, _)| *id != self.fence_id.load(Ordering::Relaxed));
    }
}

/// Refreshes every live out-fence, signaling + waking the pollers of fences
/// the host completed. Called on the refresher's active cadence.
///
/// `live` is a scratch buffer reused across calls, so the steady-state scan
/// allocates nothing. Dead registry entries (dropped `SyncFile`s) are pruned
/// in the same pass — an upgrade failing inside the lock removes the entry —
/// so the active path needs no separate prune scan.
fn refresh_all_fences(live: &mut Vec<Arc<SyncFile>>) -> usize {
    // Upgrade the live fences under the registry lock, then refresh *outside*
    // it: the lock is IRQ-save, so holding it across the GPU device lock
    // (taken by the fence query) could deadlock if a preempted task held the
    // device lock with IRQs disabled. The upgrade itself is a plain atomic
    // refcount bump and touches no other lock.
    live.clear();
    FENCE_WAITERS.lock().retain(|(_, w)| match w.upgrade() {
        Some(sf) => {
            live.push(sf);
            true
        }
        None => false,
    });
    let mut signaled = 0;
    for sf in live.iter() {
        let before = sf.signaled.load(Ordering::Acquire);
        sf.refresh();
        if !before && sf.signaled.load(Ordering::Acquire) {
            signaled += 1;
        }
    }
    signaled
}

/// Removes dead entries (dropped `SyncFile`s) from the registry so the idle
/// scan stays bounded. The active scan prunes in its upgrade pass
/// ([`refresh_all_fences`]); this is only for the idle backstop.
fn prune_dead_waiters() {
    FENCE_WAITERS.lock().retain(|(_, w)| w.strong_count() > 0);
}

/// Whether any live out-fence still waits for its host completion. The
/// refresher's active cadence is only needed while this holds: a signaled
/// fence has already woken its pollers, and its registry entry only waits
/// for the fd to close (pruned by the idle backstop). Every other wait path
/// re-checks the fence level itself.
fn has_live_waiters() -> bool {
    FENCE_WAITERS.lock().iter().any(|(_, w)| {
        w.strong_count() > 0
            && w.upgrade().is_some_and(|sf| !sf.signaled.load(Ordering::Acquire))
    })
}

/// Background fence waiter: while at least one live out-fence exists, pump +
/// refresh on the active cadence so a poll-blocked waiter observes host
/// completions even when the completion IRQ is not delivered in this
/// environment. With none, fall back to the idle backstop that only prunes
/// dead registry entries. Runs forever; spawned once by [`ensure_refresher`].
fn refresher_loop() -> ! {
    // Scratch for [`refresh_all_fences`], reused every tick: the steady-state
    // active scan allocates nothing.
    let mut live: Vec<Arc<SyncFile>> = Vec::new();
    loop {
        if has_live_waiters() {
            let signaled = refresh_all_fences(&mut live);
            let now = monotonic_time_nanos();
            if now < REFRESHER_BURST_UNTIL_NS.load(Ordering::Acquire) && signaled == 0 {
                // A kicked submit's completion is imminent; re-check quickly
                // so the fence signals within one host round-trip.
                sleep(REFRESHER_BURST_TICK);
            } else {
                if signaled > 0 {
                    REFRESHER_BURST_UNTIL_NS.store(0, Ordering::Release);
                }
                REFRESHER_WAKE.wait_timeout_until(REFRESHER_ACTIVE_TICK, || !has_live_waiters());
            }
        } else {
            prune_dead_waiters();
            REFRESHER_WAKE.wait_timeout_until(REFRESHER_IDLE_TICK, has_live_waiters);
        }
    }
}

/// Spawns the refresher task once. Called from [`SyncFile::register`].
fn ensure_refresher() {
    if REFRESHER_SPAWNED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Err(err) = kernel_thread_builder(String::from("fence-wait-refresher"))
        .spawn(|| refresher_loop())
    {
        // Re-arm the one-shot guard so a later `register` retries: without
        // the refresher, a guest blocked in poll() is only woken by the
        // device's completion IRQ path, which this environment may not
        // deliver.
        REFRESHER_SPAWNED.store(false, Ordering::Relaxed);
        ax_log::error!(
            "sync_file: failed to spawn fence-wait-refresher ({err:?}); poll-blocked out-fence \
             waiters will not be woken until it starts"
        );
    }
}

/// Kicks the refresher into burst mode. Called (task context) right after an
/// execbuffer submit that registered an out-fence; the host completes the
/// fenced command ~tens of µs later, and burst pumping signals the fence as
/// soon as that completion reaches the used ring.
pub(crate) fn kick_refresher() {
    // Release publishes the new deadline before the wake: the woken refresher
    // reads it with Acquire, and a stale deadline would drop this submit's
    // burst to the slow 250 µs active tick.
    REFRESHER_BURST_UNTIL_NS.store(
        monotonic_time_nanos() + REFRESHER_BURST_WINDOW_NS,
        Ordering::Release,
    );
    REFRESHER_WAKE.notify_one();
}

/// Completion-side kick, registered as axgpu's completion notifier
/// ([`ax_gpu::set_completion_notifier`]): the GPU IRQ worker has just pumped
/// completions, so any registered out-fence at or below the new completion
/// high-water mark can signal now. Wakes the refresher for an immediate scan
/// instead of letting a poll-blocked guest wait out the 250 µs active tick.
///
/// Runs in task context while the GPU worker holds the device lock, so this
/// must not touch the GPU lock again — it only stores the burst deadline and
/// wakes the refresher, exactly [`kick_refresher`].
pub fn on_gpu_completion() {
    kick_refresher();
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
            // Opcodes 0-2 (including the legacy Android-era WAIT at 0) were
            // burned in the mainline UAPI and fall through to `-ENOTTY`,
            // exactly like `sync_file_ioctl()`'s default arm; waiting is done
            // via poll, as on Linux.
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

        let status = if self.refresh() { 1 } else { 0 };
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
        // A level re-check refreshes the fence, so every poll/epoll wait
        // iteration pumps the used ring too.
        if self.refresh() {
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

// The axtest unit builds the whole kernel tree with `cfg(test)` set but no
// test harness: rustc strips every `#[test]` fn there, which would leave the
// named imports below unused under `-D warnings`. Host-only by design; the
// assertions are compile-time constants covered by the std test entry.
#[cfg(all(test, not(axtest)))]
mod tests {
    use super::{SYNC_IOC_FILE_INFO, SyncFenceInfo, SyncFileInfo, name_bytes};

    #[test]
    fn file_info_layout_matches_uapi() {
        assert_eq!(core::mem::size_of::<SyncFileInfo>(), 56);
        assert_eq!(core::mem::size_of::<SyncFenceInfo>(), 80);
    }

    #[test]
    fn name_is_nul_terminated_and_truncated() {
        let long = name_bytes(&"x".repeat(64));
        assert_eq!(long[31], 0);
        let short = name_bytes("ab");
        assert_eq!(&short[..3], b"ab\0");
    }

    #[test]
    fn file_info_ioctl_matches_uapi_encoding() {
        assert_eq!(SYNC_IOC_FILE_INFO, 0xc038_3e04);
    }
}
