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
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::Duration,
};

use super::super::drm::{
    DrmSyncobjArray, DrmSyncobjCreate, DrmSyncobjDestroy, DrmSyncobjEventfd, DrmSyncobjHandle,
    DrmSyncobjTimelineArray, DrmSyncobjTimelineWait, DrmSyncobjWait,
    DRM_SYNCOBJ_CREATE_SIGNALED, DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
    DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE, DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE,
    DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL,
    DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE,
    DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
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
/// waiters (eventfds and sync-file exports) waiting on future points.
pub(super) struct SyncobjState {
    /// Timeline watermark: every point `<= signaled_point` is complete.
    pub(super) signaled_point: u64,
    /// Eventfds registered via `DRM_IOCTL_SYNCOBJ_EVENTFD`, with the point
    /// each waits for.
    pub(super) eventfds: Vec<(u64, Arc<crate::file::event::EventFd>)>,
    /// Sync-file exports registered via `DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD`,
    /// each with the point it represents. The file is bound to the retiring
    /// device fence when the watermark reaches its point; an already-reached
    /// point exports an immediately-signaled file (fence id 0 completes on
    /// the first refresh).
    pub(super) sync_files: Vec<(u64, Arc<SyncFile>)>,
}

/// Out-syncobjs registered against one fenced EXECBUFFER. The fence id comes
/// from the device's monotonic counter; the entries advance when the device
/// reports that fence complete.
pub(super) struct PendingRetire {
    pub(super) fence_id: u64,
    pub(super) owner: u64,
    pub(super) syncobjs: Vec<(u32, u64)>,
}

/// Per-fd syncobj table: `(file_id, handle)` → shared state. Lives on
/// [`Card0`] rather than the per-fd context so syncobjs exist on
/// context-less fds (Linux creates them in `drm_syncobj_create_ioctl`
/// without any rendering context, and reaps them in `drm_release`).
pub(super) type SyncobjTable = Mutex<alloc::collections::BTreeMap<(u64, u32), Arc<Mutex<SyncobjState>>>>;

/// One drained retire entry: `(fence_id, owner, out-syncobjs)`.
type RetiredFence = (u64, u64, Vec<(u32, u64)>);

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
        // Snapshot the fence ids under the registry lock, then ask the device
        // outside every card0 lock: `fence_completed` takes the GPU lock and
        // pumps the control queue.
        let fence_ids: Vec<RetiredFence> = {
            let pending = self.pending_retires.lock();
            pending
                .iter()
                .map(|entry| (entry.fence_id, entry.owner, entry.syncobjs.clone()))
                .collect()
        };
        if fence_ids.is_empty() {
            return;
        }
        let mut retired: Vec<RetiredFence> = Vec::new();
        for (fence_id, owner, syncobjs_list) in fence_ids {
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
                retired.push((fence_id, owner, syncobjs_list));
            }
        }
        if retired.is_empty() {
            return;
        }
        {
            let mut pending = self.pending_retires.lock();
            for (fence_id, _, _) in &retired {
                pending.retain(|entry| entry.fence_id != *fence_id);
            }
        }
        // Advance the watermarks and collect the waiters to wake, all under
        // the syncobj table lock; signals happen after it is released.
        let table = self.syncobjs.lock();
        let mut woken: Vec<Arc<crate::file::event::EventFd>> = Vec::new();
        for (_, owner, syncobjs_list) in retired {
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

    /// Collects the eventfds and sync-files whose registered points the
    /// watermark now covers. Sync-files bind to fence id 0 (complete on the
    /// first refresh) — the caller advances the watermark only after the
    /// device fence already retired, so no per-file device fence is needed.
    fn collect_reached_waiters(
        state: &mut SyncobjState,
        woken: &mut Vec<Arc<crate::file::event::EventFd>>,
    ) {
        let mut i = 0;
        while i < state.eventfds.len() {
            if state.signaled_point >= state.eventfds[i].0 {
                woken.push(state.eventfds.swap_remove(i).1);
            } else {
                i += 1;
            }
        }
        let mut j = 0;
        while j < state.sync_files.len() {
            if state.signaled_point >= state.sync_files[j].0 {
                let (file_point, file) = state.sync_files.swap_remove(j);
                let _ = file_point;
                file.bind_fence_id(0);
                file.register();
            } else {
                j += 1;
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
                sync_files: Vec::new(),
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
        // The tail word (Linux names it pad; QUERY's LAST_SUBMITTED flag is
        // not implemented and is rejected here) and an empty handle list
        // are EINVAL, like `drm_syncobj_query_ioctl`.
        if args.flags != 0 {
            return Err(VfsError::InvalidInput);
        }
        let handles: Vec<u32> = vm_load(
            current,
            args.handles as *const u32,
            args.count_handles as usize,
        )
        .map_err(|_| VfsError::BadAddress)?;
        // Retire responses may have landed since the last submit; drain them
        // so a query observes the current watermark.
        self.pump_retired_fences();
        let states = self.lookup_syncobjs(owner, &handles)?;
        for (i, state) in states.iter().enumerate() {
            let point = state.lock().signaled_point;
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
        let handles: Vec<u32> = vm_load(
            current,
            args.handles as *const u32,
            args.count_handles as usize,
        )
        .map_err(|_| VfsError::BadAddress)?;
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
        let handles: Vec<u32> = vm_load(
            current,
            args.handles as *const u32,
            args.count_handles as usize,
        )
        .map_err(|_| VfsError::BadAddress)?;
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
        let handles: Vec<u32> = vm_load(
            current,
            args.handles as *const u32,
            args.count_handles as usize,
        )
        .map_err(|_| VfsError::BadAddress)?;
        let points: Vec<u64> =
            vm_load(current, args.points as *const u64, args.count_handles as usize)
                .map_err(|_| VfsError::BadAddress)?;
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
        let file = get_file_like(args.fd).map_err(|_| VfsError::BadFileDescriptor)?;
        let eventfd = file
            .downcast_arc::<crate::file::event::EventFd>()
            .map_err(|_| VfsError::InvalidInput)?;

        let state = self
            .syncobjs
            .lock()
            .get(&(owner, args.handle))
            .cloned()
            .ok_or(VfsError::NotFound)?;
        // `point` 0 addresses the binary view of the syncobj (Linux
        // `drm_syncobj_fence_get`); the binary view is point 1 here.
        let target = args.point.max(1);
        let mut state = state.lock();
        if args.flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE == 0
            && state.signaled_point < target
            && self.pending_fence_for(owner, args.handle, target).is_none()
        {
            // Linux resolves the fence at registration: without
            // WAIT_AVAILABLE, a point nothing backs has no fence to wait
            // on and the ioctl fails with EINVAL
            // (`drm_syncobj_eventfd_ioctl`). WAIT_AVAILABLE asks for the
            // attachment instead, which an unsubmitted point may still
            // gain.
            return Err(VfsError::InvalidInput);
        }
        // WAIT_AVAILABLE asks to be woken when a fence *attaches* to the
        // point; with the watermark model the point is always backed by a
        // fence the moment it is signaled, so the two conditions coincide.
        if state.signaled_point >= target {
            drop(state);
            let _ = eventfd.signal_kernel(1);
        } else {
            state.eventfds.push((target, eventfd));
        }
        Ok(0)
    }

    /// `DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD`: exports the syncobj's fence as a
    /// sync_file fd — the EGL native-fence path (zink/venus export per-frame
    /// completion this way to drive the output-pool buffer lifecycle). An
    /// already-reached point exports a signaled file; a future point fires
    /// when the watermark passes it. With `TIMELINE` (v6.16+) the export
    /// lands at `point`; a missing handle is ENOENT (Linux uses EINVAL only
    /// on the plain syncobj-fd export path, which this driver does not
    /// model separately).
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
            let mut state = state.lock();
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
                state.sync_files.push((point, file.clone()));
            }
        }
        let fd = add_file_like(file, true).map_err(|_| VfsError::Io)?;
        let out = DrmSyncobjHandle { fd, ..args };
        store_arg(current, arg, &out).map(|_| 0)
    }

    /// `DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE`: imports a sync_file fd — the other
    /// half of the native-fence round trip. Only files this kernel exported
    /// are honored: an already-signaled file turns into the binary-signaled
    /// syncobj view (or, with `TIMELINE`, the fence at `point`); a
    /// still-pending file has no async completion path here and is rejected
    /// rather than left silently stuck.
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
        // Linux takes a new handle regardless of flags; a bad fd or a
        // non-sync-file fd is EINVAL on both import paths.
        let file = get_file_like(args.fd).map_err(|_| VfsError::InvalidInput)?;
        let file = file
            .downcast_arc::<SyncFile>()
            .map_err(|_| VfsError::InvalidInput)?;
        if !file.refresh() {
            return Err(VfsError::InvalidInput);
        }
        let point = if args.flags & DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE != 0 {
            args.point.max(1)
        } else {
            if args.point != 0 {
                return Err(VfsError::InvalidInput);
            }
            1
        };
        let handle = self.next_syncobj_handle();
        self.syncobjs.lock().insert(
            (owner, handle),
            Arc::new(Mutex::new(SyncobjState {
                signaled_point: point,
                eventfds: Vec::new(),
                sync_files: Vec::new(),
            })),
        );
        let out = DrmSyncobjHandle { handle, ..args };
        store_arg(current, arg, &out).map(|_| 0)
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
        let handles: Vec<u32> = vm_load(
            current,
            args.handles as *const u32,
            args.count_handles as usize,
        )
        .map_err(|_| VfsError::BadAddress)?;
        let points: Vec<u64> =
            vm_load(current, args.points as *const u64, args.count_handles as usize)
                .map_err(|_| VfsError::BadAddress)?;
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
        let handles: Vec<u32> = vm_load(
            current,
            args.handles as *const u32,
            args.count_handles as usize,
        )
        .map_err(|_| VfsError::BadAddress)?;
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
