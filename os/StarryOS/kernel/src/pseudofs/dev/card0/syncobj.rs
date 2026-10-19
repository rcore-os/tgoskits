//! Generic-DRM syncobj family for `/dev/dri/card0`.
//!
//! Mesa's venus ICD tracks GPU completion through timeline syncobjs
//! (`util_sync_provider_drm` selects the kernel path once
//! `DRM_CAP_SYNCOBJ_TIMELINE` is reported), so card0 serves the family
//! alongside the `VIRTGPU_*` ioctls. v1 syncobjs are pure timelines (the only
//! form venus exercises); binary syncobj userspace is emulated on top:
//! "signaled" means `signaled_point >= 1`.
//!
//! Fence completion is observed through the device's completion view
//! ([`rdif_gpu::VirglOps::fence_completed`], which pumps the control queue as
//! a side effect). Out-syncobjs of a fenced EXECBUFFER advance only when the
//! host retired the submission's fence — signalling at submit time instead
//! would let the guest lap the venus ring and corrupt the host command
//! stream, so every reader (wait, query, export) drains the pending-retire
//! registry first.

use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use ax_std::os::arceos::task::sync::WaitQueue;
use bytemuck::{AnyBitPattern, NoUninit};
use core::{
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    time::Duration,
};

use super::super::drm::{
    DrmSyncobjArray, DrmSyncobjCreate, DrmSyncobjDestroy, DrmSyncobjEventfd, DrmSyncobjHandle,
    DrmSyncobjTimelineArray, DrmSyncobjTimelineWait, DrmSyncobjWait,
    DRM_SYNCOBJ_CREATE_SIGNALED, DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
    DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE, DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE,
    DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE, DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED,
    DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE,
    DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
};
use super::super::sync_file::SyncFile;
use super::Card0;
use crate::{
    file::{add_file_like, get_file_like},
    mm::{vm_load, vm_write_slice},
    sync::Mutex,
    task::{
        UserTaskRef,
        future::{UserWaitOutcome, block_on_user_timeout},
        kernel_thread_builder,
    },
};

type VfsResult<T = ()> = Result<T, axfs_ng_vfs::VfsError>;
use axfs_ng_vfs::VfsError;

/// A syncobj's completion state: a timeline watermark plus the registered
/// eventfd waiters waiting on future points.
pub(super) struct SyncobjState {
    /// Timeline watermark: every point `<= signaled_point` is complete.
    pub(super) signaled_point: u64,
    /// Eventfds registered via `DRM_IOCTL_SYNCOBJ_EVENTFD`.
    pub(super) eventfds: Vec<EventfdWaiter>,
}

/// One `DRM_IOCTL_SYNCOBJ_EVENTFD` registration (Linux
/// `syncobj_eventfd_entry`). Completion entries fire when the watermark
/// covers their point; `WAIT_AVAILABLE` entries fire as soon as a fence
/// *attaches* to the point (an EXECBUFFER out-syncobj registration or an
/// import), the way `drm_syncobj_add_eventfd` re-evaluates the list on
/// `drm_syncobj_add_point`/`replace_fence`.
pub(super) struct EventfdWaiter {
    pub(super) point: u64,
    pub(super) wait_available: bool,
    pub(super) eventfd: Arc<crate::file::event::EventFd>,
}

/// Out-syncobjs registered against one fenced submission (EXECBUFFER
/// out-syncobjs, or an imported sync file). The fence id comes from the
/// device's monotonic counter; the entries advance when the device reports
/// that fence complete.
pub(super) struct PendingRetire {
    /// Registration identity, unique per push. One fence id can back several
    /// registrations (a point exported and imported twice), and the pump
    /// drains by this id: removing by fence id would also delete
    /// registrations pushed while the pump queried the device outside the
    /// lock, dropping points that were never advanced.
    pub(super) id: u64,
    pub(super) fence_id: u64,
    pub(super) owner: u64,
    pub(super) syncobjs: Vec<(u32, u64)>,
}

/// Registration ids for [`PendingRetire`], allocated per push: fence ids do
/// not identify a registration (see the field docs).
static PENDING_RETIRE_NEXT_ID: AtomicU64 = AtomicU64::new(0);

impl PendingRetire {
    pub(super) fn new(fence_id: u64, owner: u64, syncobjs: Vec<(u32, u64)>) -> Self {
        Self {
            id: PENDING_RETIRE_NEXT_ID.fetch_add(1, Ordering::Relaxed),
            fence_id,
            owner,
            syncobjs,
        }
    }
}

/// Per-fd syncobj table: `(file_id, handle)` → shared state. Lives on
/// [`Card0`] rather than the per-fd context so syncobjs exist on
/// context-less fds (Linux creates them in `drm_syncobj_create_ioctl`
/// without any rendering context, and reaps them in `drm_release`).
pub(super) type SyncobjTable = Mutex<alloc::collections::BTreeMap<(u64, u32), Arc<Mutex<SyncobjState>>>>;

/// One drained retire entry: `(registration id, fence_id, owner,
/// out-syncobjs)`.
type RetiredFence = (u64, u64, u64, Vec<(u32, u64)>);

// ---- completion-side pump ----
//
// `pump_retired_fences` is the only path that advances out-syncobj
// watermarks, and every call site used to live inside a syncobj ioctl or
// the EXECBUFFER entry: a client that submits and then parks in
// `poll(eventfd)` (Mesa's DRM sync provider completion wakeup) executes no
// further ioctls, so a host retire would sit unobserved forever. This
// background pump is the completion-side driver: it drains pending retires
// on the same cadence the out-fence refresher uses, and the GPU completion
// notifier kicks it out of the idle tick within µs of a retire landing.

/// Parks the retire pump between rounds; kicked by submits and by the GPU
/// completion notifier.
static RETIRE_PUMP_WAKE: WaitQueue = WaitQueue::new();
/// One-shot guard so the pump task is spawned exactly once.
static RETIRE_PUMP_SPAWNED: AtomicBool = AtomicBool::new(false);
/// The device's card0 (single instantiation per boot, see `Card0::new`).
/// The pump only holds a weak reference, so the registry never keeps the
/// device alive.
static RETIRE_PUMP_TARGET: Mutex<Option<Weak<Card0>>> = Mutex::new(None);

/// Active cadence while any retire is outstanding — mirrors the out-fence
/// refresher's active tick (`sync_file::REFRESHER_ACTIVE_TICK`) so eventfd
/// waiters and sync-file pollers observe a retire on the same bound.
const RETIRE_PUMP_ACTIVE_TICK: Duration = Duration::from_micros(250);
/// Idle backstop: only re-checks for outstanding retires; 20 wakes/s do
/// not contend for the scheduler.
const RETIRE_PUMP_IDLE_TICK: Duration = Duration::from_millis(50);
/// Per-iteration sleep bounding syncobj waits (`wait_syncobj_dependency`
/// and both WAIT ioctls): re-park instead of spinning on the syncobj and
/// GPU locks, at the cost of up to one tick of staleness when another
/// thread advances a watermark.
const DEPENDENCY_WAIT_TICK: Duration = Duration::from_micros(250);

/// Whether any Card0 still has an unretired pending entry: the pump's
/// active cadence is only needed while this holds.
fn retire_pump_pending() -> bool {
    RETIRE_PUMP_TARGET
        .lock()
        .as_ref()
        .and_then(Weak::upgrade)
        .is_some_and(|card0| !card0.pending_retires.lock().is_empty())
}

/// Background syncobj retire pump: while any retire is outstanding, drain
/// it on the active cadence (signalling eventfd waiters and advancing
/// out-syncobj watermarks the moment the host retires a fence); otherwise
/// sleep until kicked or the idle backstop fires. Runs forever; spawned
/// once by [`ensure_retire_pump`] from `Card0::new`.
fn retire_pump_loop() -> ! {
    loop {
        if retire_pump_pending() {
            if let Some(card0) = RETIRE_PUMP_TARGET.lock().as_ref().and_then(Weak::upgrade) {
                card0.pump_retired_fences();
            }
            RETIRE_PUMP_WAKE.wait_timeout_until(RETIRE_PUMP_ACTIVE_TICK, || !retire_pump_pending());
        } else {
            RETIRE_PUMP_WAKE.wait_timeout_until(RETIRE_PUMP_IDLE_TICK, retire_pump_pending);
        }
    }
}

/// Spawns the retire pump task once. Called from `Card0::new`.
fn ensure_retire_pump() {
    if RETIRE_PUMP_SPAWNED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Err(err) =
        kernel_thread_builder(String::from("syncobj-retire-pump")).spawn(|| retire_pump_loop())
    {
        // Re-arm the one-shot guard so a later boot path retries: without
        // the pump, eventfd waiters still advance through the ioctl entry
        // points (the historical behavior).
        RETIRE_PUMP_SPAWNED.store(false, Ordering::Relaxed);
        ax_log::error!(
            "card0: failed to spawn syncobj-retire-pump ({err:?}); eventfd waiters depend on \
             ioctl entry points until it starts"
        );
    }
}

/// Registers the device's card0 and starts the pump (`Card0::new`).
pub(super) fn register_retire_pump_target(card0: Weak<Card0>) {
    *RETIRE_PUMP_TARGET.lock() = Some(card0);
    ensure_retire_pump();
}

/// Wakes the retire pump: called right after an EXECBUFFER registers a
/// pending retire, and from the GPU completion notifier — the same
/// trigger pair `kick_refresher` uses for out-fence pollers.
pub(super) fn kick_retire_pump() {
    RETIRE_PUMP_WAKE.notify_one();
}

/// The axgpu completion notifier registered by `Card0::new`: chains the
/// out-fence refresher kick with the syncobj retire-pump wake. Runs in
/// task context while the GPU worker holds the device lock, so like
/// `sync_file::on_gpu_completion` it must not touch the GPU lock itself —
/// it only wakes the pump task, which takes the lock on its own.
pub fn on_gpu_completion() {
    super::super::sync_file::on_gpu_completion();
    kick_retire_pump();
}

fn load_arg<T: AnyBitPattern>(current: &UserTaskRef, arg: usize) -> VfsResult<T> {
    let loaded = vm_load::<T>(current, arg as *const T, 1).map_err(|_| VfsError::BadAddress)?;
    Ok(loaded[0])
}

/// Upper bound for every user-supplied syncobj handle/point array. Linux
/// copies these arrays with `memdup_user`, where an oversized allocation
/// fails with ENOMEM; `vm_load` instead allocates the whole buffer before
/// touching user memory and a kernel allocation failure is fatal, so an
/// unbounded `count_handles` (u32) would let one ioctl demand a huge kernel
/// allocation. Matches the `MAX_EXEC_SYNCOBJS` cap on the venus EXECBUFFER
/// side (virtgpu.rs).
const MAX_SYNCOBJ_HANDLES: usize = 1024;

/// Loads a `count`-long `T` array for a syncobj ioctl: the count is bounded
/// (EINVAL) before any allocation, and a bad pointer maps to EFAULT.
fn load_syncobj_array<T: AnyBitPattern>(
    current: &UserTaskRef,
    ptr: u64,
    count: u32,
) -> VfsResult<Vec<T>> {
    if count as usize > MAX_SYNCOBJ_HANDLES {
        return Err(VfsError::InvalidInput);
    }
    vm_load(current, ptr as *const T, count as usize).map_err(|_| VfsError::BadAddress)
}

fn store_arg<T: NoUninit>(current: &UserTaskRef, arg: usize, value: &T) -> VfsResult {
    vm_write_slice(current, arg as *mut T, core::slice::from_ref(value))
        .map_err(|_| VfsError::BadAddress)
}

impl Card0 {
    /// Drains completed fences and advances the out-syncobjs whose
    /// submissions they retire. Every syncobj-observing entry point calls
    /// this first so a query or wait observes the current watermark; a
    /// response nobody drains would leave waiters spinning forever.
    pub(super) fn pump_retired_fences(&self) {
        // Snapshot the registrations under the registry lock, then ask the
        // device outside every card0 lock: `fence_completed` takes the GPU
        // lock and pumps the control queue.
        let registrations: Vec<RetiredFence> = {
            let pending = self.pending_retires.lock();
            pending
                .iter()
                .map(|entry| {
                    (
                        entry.id,
                        entry.fence_id,
                        entry.owner,
                        entry.syncobjs.clone(),
                    )
                })
                .collect()
        };
        if registrations.is_empty() {
            return;
        }
        let mut retired: Vec<RetiredFence> = Vec::new();
        for (id, fence_id, owner, syncobjs_list) in registrations {
            let done = ax_gpu::with_gpu(|device| {
                device
                    .virgl()
                    .ok_or(rdif_gpu::GpuError::Unsupported)?
                    .fence_completed(fence_id)
            })
            .ok()
            .and_then(|result| result.ok())
            .unwrap_or(false);
            if done {
                retired.push((id, fence_id, owner, syncobjs_list));
            }
        }
        if retired.is_empty() {
            return;
        }
        // Retire exactly the snapshot entries the device confirmed, matched
        // by registration id — never by fence id. A fence id is shared by
        // every registration of the same submission (a point exported and
        // imported twice), so a fence-id match would also remove
        // registrations pushed after the snapshot, while this pump held no
        // lock and asked the device — deleting points nothing has advanced
        // yet. A surviving same-fence registration drains on the next pump
        // round (its fence is already complete).
        {
            let mut pending = self.pending_retires.lock();
            pending.retain(|entry| !retired.iter().any(|&(id, ..)| id == entry.id));
        }
        // Advance the watermarks and collect the waiters to wake, all under
        // the syncobj table lock; signals happen after it is released.
        let table = self.syncobjs.lock();
        let mut woken: Vec<Arc<crate::file::event::EventFd>> = Vec::new();
        for (_, _, owner, syncobjs_list) in retired {
            for (handle, point) in syncobjs_list {
                let Some(state) = table.get(&(owner, handle)).cloned() else {
                    // The fd's syncobj was destroyed while the submit was in
                    // flight (Linux: a destroyed syncobj's fences still
                    // retire; nobody observes them).
                    continue;
                };
                let mut state = state.lock();
                if state.signaled_point >= point {
                    continue;
                }
                state.signaled_point = point;
                Self::collect_reached_waiters(&mut state, &mut woken);
            }
        }
        drop(table);
        for eventfd in woken {
            let _ = eventfd.signal_kernel(1);
        }
    }

    /// Collects the eventfds whose registered points the watermark now
    /// covers. Both kinds fire here: completion implies availability.
    fn collect_reached_waiters(
        state: &mut SyncobjState,
        woken: &mut Vec<Arc<crate::file::event::EventFd>>,
    ) {
        let mut i = 0;
        while i < state.eventfds.len() {
            if state.signaled_point >= state.eventfds[i].point {
                let waiter = state.eventfds.swap_remove(i);
                woken.push(waiter.eventfd);
            } else {
                i += 1;
            }
        }
    }

    /// Fires the `WAIT_AVAILABLE` eventfds whose points a submission just
    /// backed (Linux `drm_syncobj_add_eventfd` re-running the entry when
    /// `drm_syncobj_add_point` attaches a fence). Completion entries stay
    /// parked — they fire from `collect_reached_waiters` when the host
    /// retires the fence.
    pub(super) fn fire_available_waiters(
        &self,
        owner: u64,
        backed: &[(u32, u64)],
        woken: &mut Vec<Arc<crate::file::event::EventFd>>,
    ) {
        let table = self.syncobjs.lock();
        for &(handle, point) in backed {
            let Some(state) = table.get(&(owner, handle)).cloned() else {
                continue;
            };
            let mut state = state.lock();
            let mut i = 0;
            while i < state.eventfds.len() {
                if state.eventfds[i].wait_available && point >= state.eventfds[i].point {
                    let waiter = state.eventfds.swap_remove(i);
                    woken.push(waiter.eventfd);
                } else {
                    i += 1;
                }
            }
        }
    }

    /// The device fence id of the outstanding submission that will advance
    /// `handle` to `point` on `owner`'s fd, if one is still pending.
    fn pending_fence_for(&self, owner: u64, handle: u32, point: u64) -> Option<u64> {
        let pending = self.pending_retires.lock();
        pending
            .iter()
            .find(|entry| {
                entry.owner == owner
                    && entry.syncobjs.iter().any(|&(h, p)| h == handle && p == point)
            })
            .map(|entry| entry.fence_id)
    }

    /// The newest point ever attached to `handle`: the completed watermark
    /// or the highest still-pending out point, whichever is further. This is
    /// QUERY's `LAST_SUBMITTED` view (Linux reports the newest chain node's
    /// seqno, submitted or not).
    fn last_submitted_point(&self, owner: u64, handle: u32, signaled: u64) -> u64 {
        let pending = self.pending_retires.lock();
        pending
            .iter()
            .filter(|entry| entry.owner == owner)
            .filter_map(|entry| {
                entry
                    .syncobjs
                    .iter()
                    .find(|&&(h, _)| h == handle)
                    .map(|&(_, point)| point)
            })
            .fold(signaled, u64::max)
    }

    /// Advances one syncobj's watermark on behalf of an explicit
    /// `TIMELINE_SIGNAL` (or binary SIGNAL) and wakes its waiters. Also the
    /// immediate-advance path for out-syncobjs of a synchronously-completed
    /// EXECBUFFER (no retire to wait for).
    pub(super) fn signal_syncobj_point(
        &self,
        owner: u64,
        handle: u32,
        point: u64,
        woken: &mut Vec<Arc<crate::file::event::EventFd>>,
    ) {
        let table = self.syncobjs.lock();
        let Some(state) = table.get(&(owner, handle)).cloned() else {
            return;
        };
        drop(table);
        let mut state = state.lock();
        if state.signaled_point >= point {
            return;
        }
        state.signaled_point = point;
        Self::collect_reached_waiters(&mut state, woken);
    }

    /// Resolves handles to their per-fd syncobj states, failing with
    /// `NotFound` when any handle is unknown.
    pub(super) fn lookup_syncobjs(
        &self,
        owner: u64,
        handles: &[u32],
    ) -> VfsResult<Vec<Arc<Mutex<SyncobjState>>>> {
        let table = self.syncobjs.lock();
        handles
            .iter()
            .map(|handle| {
                table
                    .get(&(owner, *handle))
                    .cloned()
                    .ok_or(VfsError::NotFound)
            })
            .collect()
    }

    // ---- ioctl handlers ----

    pub(super) fn handle_syncobj_create(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let args: DrmSyncobjCreate = load_arg(current, arg)?;
        if args.flags & !DRM_SYNCOBJ_CREATE_SIGNALED != 0 {
            return Err(VfsError::InvalidInput);
        }
        let handle = self.next_syncobj_handle();
        // Syncobjs are GEM-level objects: any drm fd may create them, with
        // or without a rendering context (Linux has no context check in
        // `drm_syncobj_create`).
        self.syncobjs.lock().insert(
            (owner, handle),
            Arc::new(Mutex::new(SyncobjState {
                signaled_point: u64::from(args.flags & DRM_SYNCOBJ_CREATE_SIGNALED != 0),
                eventfds: Vec::new(),
            })),
        );
        store_arg(current, arg, &DrmSyncobjCreate {
            handle,
            flags: args.flags,
        })?;
        Ok(0)
    }

    pub(super) fn handle_syncobj_destroy(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let args: DrmSyncobjDestroy = load_arg(current, arg)?;
        if args.pad != 0 {
            return Err(VfsError::InvalidInput);
        }
        self.syncobjs
            .lock()
            .remove(&(owner, args.handle))
            .ok_or(VfsError::NotFound)?;
        Ok(0)
    }

    pub(super) fn handle_syncobj_query(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let args: DrmSyncobjTimelineArray = load_arg(current, arg)?;
        // Linux `drm_syncobj_query_ioctl`: `LAST_SUBMITTED` is the only
        // legal flag, and an empty handle list is EINVAL (unlike the WAIT
        // ioctls, which return 0 there).
        if args.flags & !DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED != 0
            || args.count_handles == 0
        {
            return Err(VfsError::InvalidInput);
        }
        let last_submitted = args.flags & DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED != 0;
        let handles: Vec<u32> = load_syncobj_array(current, args.handles, args.count_handles)?;
        // Retire responses may have landed since the last submit; drain them
        // so a query observes the current watermark.
        self.pump_retired_fences();
        let states = self.lookup_syncobjs(owner, &handles)?;
        for (i, state) in states.iter().enumerate() {
            let point = if last_submitted {
                self.last_submitted_point(owner, handles[i], state.lock().signaled_point)
            } else {
                state.lock().signaled_point
            };
            vm_write_slice(current, (args.points + i as u64 * 8) as *mut u64, &[point])
                .map_err(|_| VfsError::BadAddress)?;
        }
        Ok(0)
    }

    pub(super) fn handle_syncobj_reset(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let args: DrmSyncobjArray = load_arg(current, arg)?;
        // Linux `drm_syncobj_reset_ioctl`: a nonzero tail word or an empty
        // handle list is EINVAL.
        if args.flags != 0 || args.count_handles == 0 {
            return Err(VfsError::InvalidInput);
        }
        let handles: Vec<u32> = load_syncobj_array(current, args.handles, args.count_handles)?;
        for state in self.lookup_syncobjs(owner, &handles)? {
            state.lock().signaled_point = 0;
        }
        Ok(0)
    }

    pub(super) fn handle_syncobj_signal(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let args: DrmSyncobjArray = load_arg(current, arg)?;
        // Linux `drm_syncobj_signal_ioctl`: a nonzero tail word or an empty
        // handle list is EINVAL.
        if args.flags != 0 || args.count_handles == 0 {
            return Err(VfsError::InvalidInput);
        }
        let handles: Vec<u32> = load_syncobj_array(current, args.handles, args.count_handles)?;
        let mut woken = Vec::new();
        for handle in handles {
            self.signal_syncobj_point(owner, handle, 1, &mut woken);
        }
        // Signal the collected waiters after every lock is released, exactly
        // like the retire pump and the timeline signal path; dropping them
        // silently would park an EVENTFD waiter forever (its registration
        // was already removed by `collect_reached_waiters`).
        for eventfd in woken {
            let _ = eventfd.signal_kernel(1);
        }
        Ok(0)
    }

    pub(super) fn handle_syncobj_timeline_signal(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let args: DrmSyncobjTimelineArray = load_arg(current, arg)?;
        // Linux `drm_syncobj_timeline_signal_ioctl`: a nonzero tail word or
        // an empty handle list is EINVAL.
        if args.flags != 0 || args.count_handles == 0 {
            return Err(VfsError::InvalidInput);
        }
        let handles: Vec<u32> = load_syncobj_array(current, args.handles, args.count_handles)?;
        let points: Vec<u64> = load_syncobj_array(current, args.points, args.count_handles)?;
        let mut woken = Vec::new();
        for (handle, point) in handles.iter().zip(points) {
            self.signal_syncobj_point(owner, *handle, point, &mut woken);
        }
        // Signal the collected waiters after every lock is released, exactly
        // like the retire pump; dropping them silently would park an
        // EVENTFD waiter forever.
        for eventfd in woken {
            let _ = eventfd.signal_kernel(1);
        }
        Ok(0)
    }

    /// `DRM_IOCTL_SYNCOBJ_EVENTFD` (v6.7 UAPI): registers an eventfd that
    /// the kernel signals when a syncobj point completes. Mesa's DRM sync
    /// provider uses this as its completion wakeup, so a missing handler
    /// makes every Vulkan fence wait fall back to polling or fail outright.
    ///
    /// Linux `drm_syncobj_eventfd_ioctl` validates flags/pad/handle/eventfd
    /// and nothing more: a point no fence backs yet still registers, and
    /// the entry fires once a fence lands on the point and retires (the
    /// `syncobj_eventfd_entry` re-evaluates on every
    /// `drm_syncobj_add_point`/`replace_fence`). `WAIT_AVAILABLE` entries
    /// fire when the fence *attaches* — here, when an EXECBUFFER
    /// out-syncobj or an import backs the point ([`Self::fire_available_waiters`]).
    pub(super) fn handle_syncobj_eventfd(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let args: DrmSyncobjEventfd = load_arg(current, arg)?;
        if args.pad != 0 || args.flags & !DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE != 0 {
            return Err(VfsError::InvalidInput);
        }
        // Linux resolves the handle before the eventfd (`drm_syncobj_find`
        // then `eventfd_ctx_fdget`): an unknown handle is ENOENT even when
        // the fd is also bad.
        let state = self
            .syncobjs
            .lock()
            .get(&(owner, args.handle))
            .cloned()
            .ok_or(VfsError::NotFound)?;
        let file = get_file_like(args.fd).map_err(|_| VfsError::BadFileDescriptor)?;
        let eventfd = file
            .downcast_arc::<crate::file::event::EventFd>()
            .map_err(|_| VfsError::InvalidInput)?;
        // `point` 0 addresses the binary view of the syncobj (Linux
        // `drm_syncobj_fence_get`); the binary view is point 1 here.
        let target = args.point.max(1);
        let wait_available = args.flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE != 0;
        let mut fire_now = false;
        {
            let mut state = state.lock();
            if state.signaled_point >= target {
                // The point already completed: Linux finds the fence
                // signaled at registration (`dma_fence_add_callback`
                // returning -ENOENT) and signals immediately.
                fire_now = true;
            } else if wait_available
                && self.pending_fence_for(owner, args.handle, target).is_some()
            {
                // The point is backed by an outstanding submission: the
                // fence is already attached, so a WAIT_AVAILABLE entry
                // fires right away (Linux `syncobj_eventfd_entry_func` on
                // the WAIT_AVAILABLE path).
                fire_now = true;
            } else {
                state.eventfds.push(EventfdWaiter {
                    point: target,
                    wait_available,
                    eventfd: eventfd.clone(),
                });
            }
        }
        if fire_now {
            let _ = eventfd.signal_kernel(1);
        }
        Ok(0)
    }

    /// `DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD`: exports the syncobj's fence as a
    /// sync_file fd — the EGL native-fence path (zink/venus export per-frame
    /// completion this way to drive the output-pool buffer lifecycle); the
    /// export requires `EXPORT_SYNC_FILE`, because the no-flag direction
    /// hands out the syncobj's own fd in Linux and this driver models no
    /// syncobj fds (see `handle_syncobj_fd_to_handle`). An
    /// already-reached point exports a signaled file; a point backed by an
    /// outstanding submission binds to that submission's device fence and
    /// signals when the host retires it; a point nothing backs is `EINVAL`
    /// (Linux `drm_syncobj_find_fence` without WAIT_FOR_SUBMIT semantics —
    /// a parked file would read as fence id 0 and fake completion on the
    /// first poll). With `TIMELINE` (v6.16+) the export lands at `point`; a
    /// missing handle is ENOENT (Linux uses EINVAL only on the plain
    /// syncobj-fd export path, which this driver does not model
    /// separately).
    pub(super) fn handle_syncobj_handle_to_fd(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let args: DrmSyncobjHandle = load_arg(current, arg)?;
        if args.pad != 0
            || args.flags
                & !(DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE
                    | DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE)
                    != 0
        {
            return Err(VfsError::InvalidInput);
        }
        // Without EXPORT_SYNC_FILE Linux exports the syncobj's own fd
        // (`drm_syncobj_handle_to_fd`), an object this driver never hands
        // out (the mirror of `handle_syncobj_fd_to_handle`'s no-flag
        // rejection). Failing fast beats handing back a sync_file whose
        // type contradicts the request and breaks the standard
        // HANDLE_TO_FD → FD_TO_HANDLE round trip.
        if args.flags & DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE == 0 {
            return Err(VfsError::InvalidInput);
        }
        let timeline = args.flags & DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE != 0;
        if !timeline && args.point != 0 {
            return Err(VfsError::InvalidInput);
        }
        // The binary view of the syncobj is point 1.
        let point = if timeline { args.point.max(1) } else { 1 };
        // Drain retire responses first: exporting a point the host already
        // retired must produce a signaled file, not a pending registration.
        self.pump_retired_fences();
        let state = self
            .syncobjs
            .lock()
            .get(&(owner, args.handle))
            .cloned()
            .ok_or(VfsError::NotFound)?;
        let file = Arc::new(SyncFile::new(0));
        {
            let state = state.lock();
            if state.signaled_point >= point {
                // The point is already reached: publish the signaled state
                // directly (Linux hands out a signaled stub fence for
                // completed points) instead of waiting for the refresher
                // to observe device fence 0.
                file.mark_signaled();
                file.register();
            } else if let Some(fence_id) = self.pending_fence_for(owner, args.handle, point) {
                // The point belongs to an outstanding submission: bind the
                // export to that submission's device fence and hand it to
                // the completion refresher. The file then signals exactly
                // when the host retires the batch — without this, signaling
                // depends on somebody else issuing another syncobj ioctl to
                // run the retire pump, and a client parked on the exported
                // fd would stall forever.
                drop(state);
                file.bind_fence_id(fence_id);
                file.register();
            } else {
                // An unbacked point has no fence to export: Linux resolves
                // the fence at `drm_syncobj_export_sync_file` time and
                // fails with EINVAL (`drm_syncobj_find_fence` without
                // WAIT_FOR_SUBMIT semantics). Parking the file instead
                // would read as fence id 0 — signaled on the first poll
                // (`fence_completed(0)` is trivially true) — a fake
                // completion for GPU work that does not exist.
                return Err(VfsError::InvalidInput);
            }
        }
        let fd = add_file_like(file, true).map_err(|_| VfsError::Io)?;
        let out = DrmSyncobjHandle { fd, ..args };
        store_arg(current, arg, &out).map(|_| 0)
    }

    /// `DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE`: imports a sync_file fd — the other
    /// half of the native-fence round trip. With `IMPORT_SYNC_FILE` the
    /// `handle` field names the *existing* syncobj the fence lands in
    /// (Linux `drm_syncobj_import_sync_file_fence`: an unknown handle is
    /// ENOENT, `point` 0 replaces the syncobj's whole view, and no handle
    /// is written back). An already-signaled file completes the target
    /// point at once; a still-pending file completes it when the host
    /// retires the fence it was exported from (the same pending-retire
    /// path EXECBUFFER out-syncobjs use).
    ///
    /// The flagless direction is EINVAL: Linux requires a *syncobj* fd
    /// there (`drm_syncobj_fd_to_handle`'s fops check), and this driver
    /// never hands those out, so every fd a guest could pass fails the
    /// check exactly as it would on Linux.
    pub(super) fn handle_syncobj_fd_to_handle(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let args: DrmSyncobjHandle = load_arg(current, arg)?;
        if args.pad != 0
            || args.flags
                & !(DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE
                    | DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE)
                    != 0
        {
            return Err(VfsError::InvalidInput);
        }
        if args.flags & DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE == 0 {
            return Err(VfsError::InvalidInput);
        }
        // A bad fd or a non-sync-file fd is EINVAL (Linux
        // `sync_file_get_fence`); only then does the import target handle
        // resolve, missing ones being ENOENT (`drm_syncobj_find`) — the
        // same order the Linux helper checks in.
        let file = get_file_like(args.fd).map_err(|_| VfsError::InvalidInput)?;
        let file = file
            .downcast_arc::<SyncFile>()
            .map_err(|_| VfsError::InvalidInput)?;
        let state = self
            .syncobjs
            .lock()
            .get(&(owner, args.handle))
            .cloned()
            .ok_or(VfsError::NotFound)?;
        // With TIMELINE the fence lands at `point` (ignored otherwise, like
        // Linux reading args->point only under the flag); point 0 addresses
        // the binary view (point 1). The watermark only moves forward:
        // importing below the current watermark changes nothing, which
        // matches every observable Linux behavior for this model.
        let target = if args.flags & DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE != 0 {
            args.point.max(1)
        } else {
            1
        };
        // Resolve the imported fence: refresh pumps the completion view, so
        // a fence that retired since the export signals right here.
        if file.refresh() {
            let mut woken = Vec::new();
            self.signal_syncobj_point(owner, args.handle, target, &mut woken);
            for eventfd in woken {
                let _ = eventfd.signal_kernel(1);
            }
        } else if state.lock().signaled_point < target {
            // The fence is still retiring on the device: the point
            // completes when the host retires it — the same registration
            // EXECBUFFER out-syncobjs live under, driven by the retire
            // pump. Registering first lets a concurrent EVENTFD ioctl see
            // the backing through `pending_fence_for`; WAIT_AVAILABLE
            // waiters fire right after.
            let backed = [(args.handle, target)];
            let mut woken = Vec::new();
            self.pending_retires.lock().push_back(PendingRetire::new(
                file.bound_fence_id(),
                owner,
                Vec::from(backed),
            ));
            self.fire_available_waiters(owner, &backed, &mut woken);
            kick_retire_pump();
            for eventfd in woken {
                let _ = eventfd.signal_kernel(1);
            }
        }
        Ok(0)
    }

    /// Waits until `point` on `handle` is signalled, pumping host retires —
    /// the inline EXECBUFFER dependency wait (Linux
    /// `virtio_gpu_do_fence_wait`). Ring batches retire out of order, so
    /// enqueue order alone does not order execution and the wait is real.
    ///
    /// Linux resolves dependencies at parse time: a point that is neither
    /// reached nor backed by an outstanding retire has no fence to wait on
    /// and the submit fails with `EINVAL` instead of parking the client
    /// (`virtio_gpu_parse_deps` finding no fence). The wait itself is
    /// interruptible like `dma_fence_wait(fence, true)`: a deliverable
    /// signal aborts it with `EINTR` rather than making the task
    /// unkillable, and the sleeps replace the old `yield_now` busy loop so
    /// a stalled dependency no longer burns a CPU.
    pub(super) fn wait_syncobj_dependency(
        &self,
        owner: u64,
        current: &UserTaskRef,
        handle: u32,
        point: u64,
    ) -> VfsResult {
        let Ok(states) = self.lookup_syncobjs(owner, &[handle]) else {
            return Err(VfsError::NotFound);
        };
        // Point 0 addresses the binary view (point 1) — the EXECBUFFER
        // dependency tail carries 0 for binary syncobjs.
        let point = point.max(1);
        let state = &states[0];
        if state.lock().signaled_point < point
            && self.pending_fence_for(owner, handle, point).is_none()
        {
            return Err(VfsError::InvalidInput);
        }
        let start = ax_runtime::hal::time::monotonic_time_nanos();
        let mut warned = false;
        loop {
            self.pump_retired_fences();
            if state.lock().signaled_point >= point {
                return Ok(());
            }
            let now = ax_runtime::hal::time::monotonic_time_nanos();
            if !warned && now.saturating_sub(start) > 1_000_000_000 {
                warned = true;
                warn!("card0: execbuffer in-syncobj wait >1s (handle {handle}, point {point})");
            }
            // Bounded, interruptible sleep: re-check the watermark on every
            // wake instead of spinning on the syncobj and GPU locks.
            let outcome = block_on_user_timeout(
                current,
                Some(DEPENDENCY_WAIT_TICK),
                core::future::pending::<()>(),
            );
            if let UserWaitOutcome::Interrupted = outcome {
                return Err(VfsError::Interrupted);
            }
        }
    }

    pub(super) fn handle_syncobj_timeline_wait(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let mut args: DrmSyncobjTimelineWait = load_arg(current, arg)?;
        // Linux `drm_syncobj_timeline_wait_ioctl`: the four WAIT_* bits are
        // the whole legal flag set.
        const KNOWN_FLAGS: u32 = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL
            | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT
            | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE
            | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE;
        if args.flags & !KNOWN_FLAGS != 0 {
            return Err(VfsError::InvalidInput);
        }
        // An empty handle list is an immediate success in Linux.
        if args.count_handles == 0 {
            return Ok(0);
        }
        let handles: Vec<u32> = load_syncobj_array(current, args.handles, args.count_handles)?;
        let points: Vec<u64> = load_syncobj_array(current, args.points, args.count_handles)?;
        // Point 0 addresses the binary view of the syncobj (Linux resolves
        // the current fence); the binary view is point 1 here, so a 0 must
        // not compare as always-reached against an unsignaled watermark.
        let points: Vec<u64> = points.into_iter().map(|p| p.max(1)).collect();
        let states = self.lookup_syncobjs(owner, &handles)?;

        // Index of the first satisfied handle, for the non-WAIT_ALL
        // `first_signaled` write-back on success (Linux sets it only there).
        let mut first_signaled: Option<u32> = None;
        let mut check = || -> VfsResult<bool> {
            let mut any = false;
            let mut all = true;
            for (i, state) in states.iter().enumerate() {
                let done = state.lock().signaled_point >= points[i];
                any |= done;
                all &= done;
                if done && first_signaled.is_none() {
                    first_signaled = Some(i as u32);
                }
            }
            Ok(if args.flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL != 0 {
                all
            } else {
                any
            })
        };

        // WAIT_DEADLINE only forwards a scheduling hint (ignored here).
        // WAIT_FOR_SUBMIT and WAIT_AVAILABLE both permit blocking on a
        // point nothing has submitted yet; without either, Linux rejects an
        // unsubmitted dependency with EINVAL up front — "submitted" in this
        // model means the point is either reached or backed by an
        // outstanding retire (`pending_fence_for`).
        let allow_block = args.flags
            & (DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE)
            != 0;
        if !allow_block {
            for (i, state) in states.iter().enumerate() {
                if state.lock().signaled_point < points[i]
                    && self.pending_fence_for(owner, handles[i], points[i]).is_none()
                {
                    return Err(VfsError::InvalidInput);
                }
            }
        }

        // `timeout_nsec` is an absolute CLOCK_MONOTONIC deadline (Linux
        // `drm_timeout_abs_to_jiffies`); the field is __s64 in the UAPI, so
        // a negative value means "already lapsed" and collapses to a single
        // poll. Expiry reports ETIME.
        let deadline = u64::try_from(args.timeout_nsec).unwrap_or(0);
        let start = ax_runtime::hal::time::monotonic_time_nanos();
        let mut stall_warned = false;
        loop {
            // Drain host retire responses so a parked wait can observe
            // progress made while this thread slept between checks.
            self.pump_retired_fences();
            if check()? {
                // Linux writes the first satisfied index back on a
                // successful non-WAIT_ALL return (`first_signaled`).
                if args.flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL == 0 && let Some(index) = first_signaled {
                    args.first_signaled = index;
                    store_arg(current, arg, &args)?;
                }
                return Ok(0);
            }
            let now = ax_runtime::hal::time::monotonic_time_nanos();
            if now >= deadline {
                return Err(VfsError::TimerExpired);
            }
            // A wait spinning past one second means the retire responses for
            // its fence are not arriving — the signature of a venus-side
            // fence stall.
            if !stall_warned && now.saturating_sub(start) > 1_000_000_000 {
                stall_warned = true;
                warn!(
                    "card0: timeline syncobj wait pending >1s (handles={:?})",
                    handles
                );
            }
            // Interruptible parking (Linux `dma_fence_wait` with interrupts
            // enabled): a deliverable signal aborts the wait with EINTR
            // instead of making the task unkillable until the deadline.
            if let UserWaitOutcome::Interrupted = block_on_user_timeout(
                current,
                Some(DEPENDENCY_WAIT_TICK),
                core::future::pending::<()>(),
            ) {
                return Err(VfsError::Interrupted);
            }
        }
    }

    /// Binary `DRM_IOCTL_SYNCOBJ_WAIT` — the timeline predicate with every
    /// handle checked at point 1.
    pub(super) fn handle_syncobj_wait(
        &self,
        owner: u64,
        current: &UserTaskRef,
        arg: usize,
    ) -> VfsResult<usize> {
        let mut args: DrmSyncobjWait = load_arg(current, arg)?;
        // Linux `drm_syncobj_wait_ioctl`: WAIT_AVAILABLE is illegal here
        // (timeline-only); WAIT_DEADLINE is a hint and is accepted.
        const KNOWN_FLAGS: u32 = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL
            | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT
            | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE;
        if args.flags & !KNOWN_FLAGS != 0 {
            return Err(VfsError::InvalidInput);
        }
        if args.count_handles == 0 {
            return Ok(0);
        }
        let handles: Vec<u32> = load_syncobj_array(current, args.handles, args.count_handles)?;
        let states = self.lookup_syncobjs(owner, &handles)?;

        // Index of the first satisfied handle, for the non-WAIT_ALL
        // `first_signaled` write-back on success (Linux sets it only there).
        let mut first_signaled: Option<u32> = None;
        let mut check = || -> VfsResult<bool> {
            let mut any = false;
            let mut all = true;
            for (i, state) in states.iter().enumerate() {
                let done = state.lock().signaled_point >= 1;
                any |= done;
                all &= done;
                if done && first_signaled.is_none() {
                    first_signaled = Some(i as u32);
                }
            }
            Ok(if args.flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL != 0 {
                all
            } else {
                any
            })
        };

        let allow_block = args.flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT != 0;
        if !allow_block {
            for (i, state) in states.iter().enumerate() {
                if state.lock().signaled_point < 1
                    && self.pending_fence_for(owner, handles[i], 1).is_none()
                {
                    // An unsignaled binary syncobj nothing backs is the
                    // "fence not submitted" EINVAL of the Linux loop above.
                    return Err(VfsError::InvalidInput);
                }
            }
        }

        // Absolute CLOCK_MONOTONIC deadline; a negative or zero value has
        // already lapsed, so the first unsatisfied poll returns ETIME.
        let deadline = u64::try_from(args.timeout_nsec).unwrap_or(0);
        loop {
            self.pump_retired_fences();
            if check()? {
                // Linux writes the first satisfied index back on a
                // successful non-WAIT_ALL return (`first_signaled`).
                if args.flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL == 0 && let Some(index) = first_signaled {
                    args.first_signaled = index;
                    store_arg(current, arg, &args)?;
                }
                return Ok(0);
            }
            let now = ax_runtime::hal::time::monotonic_time_nanos();
            if now >= deadline {
                return Err(VfsError::TimerExpired);
            }
            // Interruptible parking, matching the timeline wait above.
            if let UserWaitOutcome::Interrupted = block_on_user_timeout(
                current,
                Some(DEPENDENCY_WAIT_TICK),
                core::future::pending::<()>(),
            ) {
                return Err(VfsError::Interrupted);
            }
        }
    }

    /// Removes all syncobjs of a closing file description
    /// (Linux `drm_release` reaps the fd's syncobj table).
    pub(super) fn close_syncobjs(&self, owner: u64) {
        self.syncobjs.lock().retain(|&(fd_owner, _), _| fd_owner != owner);
    }

    // ---- helpers ----

    fn next_syncobj_handle(&self) -> u32 {
        // Syncobj handles live in the per-fd tables, so a device-wide counter
        // keeps them unique across fds without colliding with GEM handles
        // (which start at the venus face's reserved range).
        static NEXT: AtomicU32 = AtomicU32::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }
}
