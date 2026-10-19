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
};
use core::{
    cell::Cell,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_lazyinit::OnceLock;

use ax_runtime::hal::time::monotonic_time_nanos;
use ax_std::os::arceos::task::sync::WaitQueue;
use axpoll::{IoEvents, Pollable};
use axpoll_set::PollSet;
use bytemuck::{AnyBitPattern, NoUninit};

use crate::{
    StarryError, StarryResult,
    file::FileLike,
    mm::{VmMutPtr, VmPtr},
    sync::RawSpinLock,
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
    /// Identity for the intrusive waiters list: lets a scanner take a strong
    /// reference to a node without allocating. Written once by
    /// [`Self::register`] right after the node is linked (a scanner reaching
    /// the node in that window simply skips it for one tick).
    self_weak: OnceLock<Weak<SyncFile>>,
    /// Whether this file is currently linked into [`FENCE_WAITERS`], and the
    /// list links themselves. Guarded by the registry lock; see
    /// [`RegistryHead`] for why the registry must never allocate.
    registered: Cell<bool>,
    prev: Cell<Option<NonNull<SyncFile>>>,
    next: Cell<Option<NonNull<SyncFile>>>,
}

// SAFETY: `self_weak` is written exactly once before the node becomes
// reachable from other threads and only read afterwards; `registered`,
// `prev` and `next` are only touched while holding `FENCE_WAITERS`, and the
// pointers they hold are only dereferenced under that lock. The remaining
// fields are atomics or `Sync` primitives, so moving a `SyncFile` across
// threads and sharing it is sound.
unsafe impl Send for SyncFile {}
unsafe impl Sync for SyncFile {}

/// The out-fence registry: an intrusive doubly-linked list threaded through
/// the registered [`SyncFile`]s themselves.
///
/// Intrusive on purpose. The registry used to be a heap `Vec` pushed to
/// **while holding the IRQ-save registry lock**; growing that Vec can extend
/// the kernel heap and reach a TLB shootdown (`flush_tlb_range_all_cpus` →
/// `ax_ipi::call_on_cpu`), which spin-waits for every CPU to acknowledge —
/// while a target CPU spinning on this very lock with IRQs masked can never
/// take the shootdown IPI. That closed a four-CPU wedge 8.4 h into a desktop
/// soak (two CPUs parked in `call_on_cpu` on `flush_tlb_range_thunk`, two
/// spinning here in `register`/`drop`). With the links inside the files no
/// registry path allocates, so a shootdown can never originate under the
/// lock again.
///
/// Soundness: while `FENCE_WAITERS` is held, every linked node's storage is
/// valid — a dying file's `Drop` must take this lock to unlink before its
/// memory is freed, so it queues on the lock instead.
///
/// The lock stays IRQ-save: the completion IRQ path must never spin on it
/// behind a task-context holder.
static FENCE_WAITERS: RawSpinLock<RegistryHead> = RawSpinLock::new(RegistryHead {
    first: None,
    last: None,
});

struct RegistryHead {
    first: Option<NonNull<SyncFile>>,
    last: Option<NonNull<SyncFile>>,
}

// SAFETY: raw pointers are not `Send` by construction; the head is only
// reachable through `FENCE_WAITERS`, whose critical sections move and
// dereference the links on one CPU at a time. Linked nodes are registered
// `SyncFile`s, which are `Send`/`Sync` through their explicit impls above.
unsafe impl Send for RegistryHead {}

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
            self_weak: OnceLock::new(),
            registered: Cell::new(false),
            prev: Cell::new(None),
            next: Cell::new(None),
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

    /// The device fence this file waits on. Syncobj imports need it to
    /// advance the importing timeline when the host retires this fence
    /// (`DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE` over a still-pending export).
    pub fn bound_fence_id(&self) -> u64 {
        self.fence_id.load(Ordering::Relaxed)
    }

    /// Registers this out-fence in the completion registry and makes sure the
    /// refresher task exists (card0's EXECBUFFER `FENCE_FD_OUT` path).
    ///
    /// Linking is allocation-free by construction (see [`RegistryHead`]).
    /// `ensure_refresher` may spawn a task and allocate, so it runs strictly
    /// after the registry lock is released — allocating under the IRQ-save
    /// lock is exactly the cross-CPU wedge the intrusive design prevents.
    ///
    /// The lock discipline is IRQ-save on both sides (here and in the
    /// scans), so the completion IRQ can never spin on the registry while a
    /// task holds it.
    pub fn register(self: &Arc<Self>) {
        let fresh = {
            let mut registry = FENCE_WAITERS.lock_irqsave();
            if self.registered.get() {
                false
            } else {
                let node = NonNull::from(self.as_ref());
                self.prev.set(None);
                self.next.set(registry.first);
                if let Some(first) = registry.first {
                    // SAFETY: linked nodes are registered files whose storage
                    // is valid under the registry lock (a dying file unlinks
                    // in `Drop` before its memory is freed).
                    unsafe { first.as_ref().prev.set(Some(node)) };
                } else {
                    registry.last = Some(node);
                }
                registry.first = Some(node);
                self.registered.set(true);
                true
            }
        };
        if fresh {
            // Both the identity write and the task spawn may take locks that
            // must never run under the IRQ-save registry lock (`init_once`
            // can wait on a concurrent initializer; spawning allocates and
            // can reach a TLB shootdown).
            self.self_weak.call_once(|| Arc::downgrade(self));
            ensure_refresher();
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
        // Unlink exactly this file's own node, by identity: fence ids are
        // shared whenever one point is exported twice (syncobj exports and
        // the reached-point stub id 0), and dropping a twin export must not
        // unregister its sibling — a poll blocked on the sibling would then
        // never be woken by the refresher scan. Unlinking is allocation-free
        // (see [`RegistryHead`]).
        let mut registry = FENCE_WAITERS.lock_irqsave();
        if !self.registered.get() {
            return;
        }
        match self.prev.get() {
            Some(prev) => {
                // SAFETY: neighbors are linked files, storage valid under
                // the registry lock.
                unsafe { prev.as_ref().next.set(self.next.get()) };
            }
            None => registry.first = self.next.get(),
        }
        match self.next.get() {
            Some(next) => {
                // SAFETY: as above.
                unsafe { next.as_ref().prev.set(self.prev.get()) };
            }
            None => registry.last = self.prev.get(),
        }
        self.registered.set(false);
    }
}

/// Refreshes every registered out-fence, signaling + waking the pollers of
/// fences the host completed. Called on the refresher's active cadence.
///
/// The walk is anchored on an `Arc`, never on a raw pointer: between two
/// lock holds the current node is refreshed outside the lock (taking the
/// GPU lock via `fence_level`), during which *any* other node may be
/// dropped — its `Drop` takes the lock, unlinks it and its memory is freed.
/// A raw cursor to such a node would therefore dangle at the next
/// relock. Holding a strong reference to the current node keeps it linked
/// and alive (an `Arc` prevents `Drop` outright), and `Arc::upgrade` is a
/// plain atomic increment, so the no-allocation-under-the-lock guarantee
/// ([`RegistryHead`]) is preserved. Raw pointers are only dereferenced
/// *inside* a lock hold, where a dying node's `Drop` is blocked before its
/// memory can be freed.
///
/// New registrations link at the front and are covered by the next tick's
/// pass; a node that dies mid-walk is skipped (its upgrade fails under the
/// lock while its `Drop` queues behind the walker).
fn refresh_all_fences() -> usize {
    let mut signaled = 0;
    let mut current: Option<Arc<SyncFile>> = {
        let registry = FENCE_WAITERS.lock_irqsave();
        first_upgradable(registry.first)
    };
    while let Some(file) = current {
        let before = file.signaled.load(Ordering::Acquire);
        file.refresh();
        if !before && file.signaled.load(Ordering::Acquire) {
            signaled += 1;
        }
        current = {
            // The guard is held across the read and the `first_upgradable`
            // walk: every raw-pointer deref below happens under it.
            let _registry = FENCE_WAITERS.lock_irqsave();
            let next = file.next.get();
            first_upgradable(next)
        };
    }
    signaled
}

/// Upgrades the first alive node at or after `start`, skipping dying nodes
/// (their `Drop` is queued on the registry lock, so their links and storage
/// stay valid to read until it acquires the lock). Called with
/// [`FENCE_WAITERS`] held.
fn first_upgradable(start: Option<NonNull<SyncFile>>) -> Option<Arc<SyncFile>> {
    let mut cursor = start;
    while let Some(node) = cursor {
        // SAFETY: under the registry lock, a linked node's storage is valid
        // even while it is dying — its `Drop` must take this lock to unlink
        // before its memory is freed, so it queues here instead.
        let this = unsafe { node.as_ref() };
        let next = this.next.get();
        if let Some(file) = this.self_weak.get().and_then(|weak| weak.upgrade()) {
            return Some(file);
        }
        cursor = next;
    }
    None
}

/// Whether any registered out-fence still waits for its host completion. The
/// refresher's active cadence is only needed while this holds: a signaled
/// fence has already woken its pollers, and an unregistered one is gone
/// (dying files unlink themselves in `Drop`; no separate prune pass exists).
/// Every other wait path re-checks the fence level itself.
fn has_live_waiters() -> bool {
    let registry = FENCE_WAITERS.lock_irqsave();
    let mut cursor = registry.first;
    while let Some(node) = cursor {
        // SAFETY: as in `refresh_all_fences`.
        let this = unsafe { node.as_ref() };
        if !this.signaled.load(Ordering::Acquire) {
            return true;
        }
        cursor = this.next.get();
    }
    false
}

/// Background fence waiter: while at least one live out-fence exists, pump +
/// refresh on the active cadence so a poll-blocked waiter observes host
/// completions even when the completion IRQ is not delivered in this
/// environment. With none, fall back to the idle backstop. Runs forever;
/// spawned once by [`ensure_refresher`].
fn refresher_loop() -> ! {
    loop {
        if has_live_waiters() {
            let signaled = refresh_all_fences();
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
            REFRESHER_WAKE.wait_timeout_until(REFRESHER_IDLE_TICK, has_live_waiters);
        }
    }
}

/// Spawns the refresher task once. Called from [`SyncFile::register`].
fn ensure_refresher() {
    if REFRESHER_SPAWNED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Err(err) =
        kernel_thread_builder(String::from("fence-wait-refresher")).spawn(|| refresher_loop())
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
