// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.
//! RDIF completion tokens bridged into the shared Linux dma-fence core.

use alloc::{boxed::Box, string::String};
use core::{
    cell::UnsafeCell,
    ffi::{c_char, c_void},
    mem::{MaybeUninit, offset_of, size_of},
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_gpu::rdif_gpu::{Completion, CompletionStatus, GpuError};
use ax_lazyinit::LazyInit;
use drm_kapi::{
    config::GFP_KERNEL,
    dma_fence::{
        dma_fence_context_alloc, dma_fence_get, dma_fence_init, dma_fence_put, dma_fence_set_error,
        dma_fence_signal,
    },
    dma_resv::{dma_resv_add_fence, dma_resv_reserve_fences},
    memory::{kfree, kmalloc},
    task_runtime,
    types::{DmaFence, DmaFenceOps, ListHead, Spinlock, WaitQueueHead, WorkStruct},
    wait::{init_waitqueue_head, wake_up},
    workqueue::{SYSTEM_DFL_WQ, init_work_c, queue_work},
};
use kdrm_core::{
    drm_crtc_types::DrmCrtc,
    drm_framebuffer_types::DrmFramebuffer,
    drm_framebuffer_upstream::{drm_framebuffer_get, drm_framebuffer_put},
    drm_gem_types::DrmGemObject,
    drm_gem_upstream::{drm_gem_object_get, drm_gem_object_put},
    drm_vblank_types::DrmPendingVblankEvent,
};

use super::presentation::send_event;

const NEG_EIO: i32 = -drm_kapi::config::EIO;

#[repr(C)]
struct RdifFence {
    base: DmaFence,
    lock: Spinlock,
    completion: Completion,
}

#[repr(C)]
struct PendingFence {
    fence: *mut DmaFence,
    next: *mut PendingFence,
    keepalive_object: *mut DrmGemObject,
    keepalive_framebuffer: *mut DrmFramebuffer,
    event_crtc: *mut DrmCrtc,
    event: *mut DrmPendingVblankEvent,
}

#[derive(Default)]
struct PendingList {
    head: usize,
    tail: usize,
}

impl PendingList {
    fn push(&mut self, node: *mut PendingFence) {
        debug_assert!(!node.is_null());
        // SAFETY: 'node' is guaranteed non-null, aligned, and pointing to a newly allocated PendingFence node.
        unsafe { (*node).next = ptr::null_mut() };
        let tail = self.tail as *mut PendingFence;
        if tail.is_null() {
            self.head = node as usize;
        } else {
            // SAFETY: 'tail' is non-null and was previously stored from a valid allocated PendingFence node.
            unsafe { (*tail).next = node };
        }
        self.tail = node as usize;
    }

    fn take(&mut self) -> *mut PendingFence {
        let head = self.head as *mut PendingFence;
        self.head = 0;
        self.tail = 0;
        head
    }
}

struct CompletionQueue {
    work: core::cell::UnsafeCell<WorkStruct>,
    pending: spin::Mutex<PendingList>,
    requested: AtomicBool,
}

// SAFETY: `work` is initialized once before publication and is only queued via
// workqueue; `pending` serializes all token ownership and `requested` only
// merges wakeups.
unsafe impl Sync for CompletionQueue {}
// SAFETY: the work item has stable storage and is only accessed through the
// workqueue API; the pending list is protected by its spin lock.
unsafe impl Send for CompletionQueue {}

impl CompletionQueue {
    fn new() -> Self {
        // SAFETY: WorkStruct is a C work-item record whose init_work_c
        // initializer writes its data/list/callback fields before queueing.
        let work = unsafe { MaybeUninit::<WorkStruct>::zeroed().assume_init() };
        Self {
            work: core::cell::UnsafeCell::new(work),
            pending: spin::Mutex::new(PendingList::default()),
            requested: AtomicBool::new(false),
        }
    }
}

static COMPLETIONS: LazyInit<CompletionQueue> = LazyInit::new();

/// Wake channel for the background completion pump thread, built from the
/// shared Linux-style wait primitives so this driver reaches the scheduler
/// exclusively through drm-kapi. Producers run in task context only (submits,
/// fence-signaling arming, and the ax-gpu completion notifier), so the queue
/// never sees hard-IRQ callers. The generation counter preserves the
/// not-lost-wakeup property across pump passes: every notify bumps it, so a
/// notify that lands between two passes makes the next park return at once
/// instead of being lost.
struct PumpWake {
    queue: UnsafeCell<WaitQueueHead>,
    generation: AtomicU64,
}

// SAFETY: the wait queue serializes its own list through the embedded
// spinlock, and the generation is a plain atomic. Only the pump thread parks
// entries on the queue; producers only walk already-linked entries.
unsafe impl Sync for PumpWake {}

static PUMP_WAKE: PumpWake = PumpWake {
    // SAFETY: the null-linked list head is never walked before `fence::init`
    // runs `PumpWake::init`: no pump thread exists before that call spawns
    // one, and producers reach the queue only through
    // `schedule_completion_worker`, which returns early until `COMPLETIONS`
    // is initialized — strictly after the queue became valid.
    queue: UnsafeCell::new(WaitQueueHead {
        lock: Spinlock::new(),
        head: ListHead {
            next: ptr::null_mut(),
            prev: ptr::null_mut(),
        },
    }),
    generation: AtomicU64::new(0),
};

impl PumpWake {
    /// Link the queue while it has no waiters or producers yet.
    fn init(&self) {
        // SAFETY: callers hold exclusive access: `fence::init` runs this
        // before the pump thread exists, and a failed spawn leaves no thread
        // that could park on or wake the queue before a retry.
        unsafe { init_waitqueue_head(&mut *self.queue.get()) };
    }

    /// Publish one notification for the pump.
    fn notify(&self) {
        self.generation.fetch_add(1, Ordering::Release);
        // SAFETY: `fence::init` linked the queue before any producer or
        // waiter could reach it; the wake walks only registered entries.
        unsafe { wake_up(self.queue.get()) };
    }

    /// Park the calling pump pass until a notification generation newer than
    /// the caller's last observed one arrives, or `jiffies` elapse.
    fn wait_for_notification(&self, jiffies: i64) {
        let seen = self.generation.load(Ordering::Acquire);
        let _ = drm_kapi::wait_event_timeout!(
            self.queue.get(),
            self.generation.load(Ordering::Acquire) != seen,
            jiffies
        );
    }
}

/// Convert a park duration to jiffies of the shared wait channel, rounding up
/// so a wait never ends before the requested duration.
const fn duration_to_jiffies(duration: Duration) -> i64 {
    let nanos = duration.as_nanos();
    let scaled = nanos.saturating_mul(drm_kapi::config::CONFIG_HZ as u128);
    let jiffies = scaled.saturating_add(999_999_999) / 1_000_000_000;
    // `Ord::min` is not const-callable on this toolchain.
    if jiffies > i64::MAX as u128 {
        i64::MAX
    } else {
        jiffies as i64
    }
}

/// Poll cadence of the completion pump while fenced work is in flight. The
/// interval doubles on each pass that confirms nothing, up to
/// [`PUMP_IDLE_POLL_JIFFIES`], and resets as soon as any fence retires.
const PUMP_ACTIVE_POLL: Duration = Duration::from_micros(250);
/// Upper bound of the pump's backoff, and its re-check period while parked
/// with an empty queue.
const PUMP_IDLE_POLL: Duration = Duration::from_millis(50);
/// Jiffy budgets of the two park shapes above. The shared wait channel is
/// tick-granular (`CONFIG_HZ` = 100), so the finest park is one tick.
const PUMP_ACTIVE_POLL_JIFFIES: i64 = duration_to_jiffies(PUMP_ACTIVE_POLL);
const PUMP_IDLE_POLL_JIFFIES: i64 = duration_to_jiffies(PUMP_IDLE_POLL);

static FENCE_OPS: DmaFenceOps = DmaFenceOps {
    get_driver_name: Some(driver_name),
    get_timeline_name: Some(timeline_name),
    enable_signaling: Some(enable_signaling),
    signaled: None,
    wait: None,
    release: Some(release_fence),
    set_deadline: None,
};

/// Name of the background pump thread started by [`init`].
const PUMP_THREAD_NAME: &str = "gpu-completion-pump";

pub(crate) fn init() -> Result<(), i32> {
    init_with(Box::new(completion_pump), completion_worker)
}

/// Bring up the completion infrastructure in the order the fence contract
/// needs: wake channel, pump thread, then the published completion queue.
///
/// Spawning the pump BEFORE publishing the queue keeps a spawn failure free
/// of half-initialized state, so retrying this function stays meaningful, and
/// lets callers fail their device probe instead of publishing a DRM node
/// whose fence waits would hang forever on a platform whose GPU delivers no
/// interrupt. The pump may begin polling before the queue exists;
/// `poll_pending_completions` treats an unpublished queue as empty and the
/// idle re-check bounds that race.
///
/// The entry and worker are parameters so the host test can drive this
/// sequence with no-op bodies instead of linking the pump's device call
/// graph into the test binary.
fn init_with(
    entry: task_runtime::TaskEntry,
    worker: unsafe extern "C" fn(*mut WorkStruct),
) -> Result<(), i32> {
    if COMPLETIONS.is_inited() {
        return Ok(());
    }
    PUMP_WAKE.init();
    // R4: a compositor's render thread parks in
    // `DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT` (WAIT_FOR_SUBMIT, INT64_MAX) holding
    // its userspace GL lock, and every other GPU ioctl caller freezes with
    // it. The completion worker is one-shot per schedule, and on a platform
    // whose GPU delivers no interrupt there is nothing left to re-poll once
    // the submit-time pass misses the (already retired) host response, so
    // out-syncobj fences never signal and the waiter sleeps forever. A
    // dedicated pump keeps re-polling while fences are pending — the same
    // shape the previous StarryOS card0 backend used for its retire pump.
    task_runtime::spawn(String::from(PUMP_THREAD_NAME), entry)?;
    COMPLETIONS.init_once(CompletionQueue::new());
    let queue = COMPLETIONS.get().expect("completion queue was initialized");
    // SAFETY: The LazyInit static gives the work record a stable address for
    // the queue's entire lifetime. The runtime workqueue provider is installed
    // before this function is called by ax-drm initialization.
    unsafe { init_work_c(&mut *queue.work.get(), worker) };
    Ok(())
}

/// This is installed as ax-gpu's sole notifier. It runs while the GPU owner
/// may hold its device lock, so it only publishes/enqueues work; no RDIF call,
/// fence callback, or user-facing completion signaling occurs here.
pub(crate) fn on_gpu_completion() {
    schedule_completion_worker();
}

pub(crate) fn new_completion(completion: Completion) -> *mut DmaFence {
    new_completion_with_owners(
        completion,
        ptr::null_mut(),
        ptr::null_mut(),
        ptr::null_mut(),
        ptr::null_mut(),
    )
}

/// Create a completion fence whose pending queue item owns `keepalive_object`
/// and `event` until RDIF has reached a terminal completion state. The caller
/// transfers one GEM reference for pending completions.
fn new_completion_with_owners(
    completion: Completion,
    keepalive_object: *mut DrmGemObject,
    keepalive_framebuffer: *mut DrmFramebuffer,
    event_crtc: *mut DrmCrtc,
    event: *mut DrmPendingVblankEvent,
) -> *mut DmaFence {
    if !COMPLETIONS.is_inited() {
        if let Completion::Pending(_) = completion {
            let result = wait_for_completion(completion);
            if result.is_err() {
                log::debug!("RDIF completion failed while the fence worker was unavailable");
            }
            // SAFETY: Pending ownership was transferred by the caller; the
            // synchronous wait reached a terminal RDIF completion state.
            unsafe {
                drm_gem_object_put(keepalive_object);
                drm_framebuffer_put(keepalive_framebuffer);
                send_event(event_crtc, event);
            }
        }
        return ptr::null_mut();
    }
    let fence = allocate_fence(completion);
    if fence.is_null() {
        if let Completion::Pending(_) = completion {
            // Do not let an OOM path report a pending operation complete.
            // The caller still owns its original resources while we wait.
            let result = wait_for_completion(completion);
            if result.is_err() {
                log::debug!("RDIF completion failed while allocating a dma-fence");
            }
            // SAFETY: Pending ownership was transferred by the caller; the
            // synchronous wait reached a terminal RDIF completion state.
            unsafe {
                drm_gem_object_put(keepalive_object);
                drm_framebuffer_put(keepalive_framebuffer);
                send_event(event_crtc, event);
            }
        }
        return ptr::null_mut();
    }
    match completion {
        // SAFETY: 'fence' was initialized above and is a valid DmaFence; signaling it marks it completed and notifies waiters.
        Completion::Complete => unsafe { dma_fence_signal(fence) },
        Completion::Pending(_) => {
            let queue = COMPLETIONS.get().expect("completion queue initialized");
            // Allocate the queue node before taking the spin lock: KAPI's
            // allocator may sleep on memory pressure.
            let node = kmalloc(size_of::<PendingFence>(), GFP_KERNEL).cast::<PendingFence>();
            if node.is_null() {
                // Falling back to an immediate signal here lets both a
                // userspace fence and object destruction outrun real device
                // use. Wait synchronously, then publish only the terminal
                // completion status.
                let result = wait_for_completion(completion);
                // SAFETY: 'fence' is a valid, live DmaFence initialized above. 'keepalive_object', 'keepalive_framebuffer',
                // 'event_crtc', and 'event' are caller-provided references whose ownership is consumed here following synchronous terminal completion.
                unsafe {
                    if result.is_err() {
                        dma_fence_set_error(fence, NEG_EIO);
                    }
                    send_event(event_crtc, event);
                    drm_gem_object_put(keepalive_object);
                    drm_framebuffer_put(keepalive_framebuffer);
                    dma_fence_signal(fence);
                }
                return fence;
            }
            // The work queue owns this extra reference until it observes the
            // RDIF completion and signals the shared fence.
            // SAFETY: `fence` was initialized above and this path still holds
            // its creation reference.
            unsafe { dma_fence_get(fence) };
            // SAFETY: 'node' was allocated with size_of::<PendingFence>() and checked non-null, properly aligned,
            // and writable. It is populated and transferred into queue.pending under its spinlock.
            unsafe {
                ptr::write(
                    node,
                    PendingFence {
                        fence,
                        next: ptr::null_mut(),
                        keepalive_object,
                        keepalive_framebuffer,
                        event_crtc,
                        event,
                    },
                );
                queue.pending.lock().push(node);
            }
            schedule_completion_worker();
        }
    }
    fence
}

fn allocate_fence(completion: Completion) -> *mut DmaFence {
    let allocation = kmalloc(size_of::<RdifFence>(), GFP_KERNEL).cast::<RdifFence>();
    if allocation.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: The KAPI allocation has the required size/alignment. Initialize
    // the external lock and immutable completion token before dma_fence_init.
    unsafe {
        ptr::addr_of_mut!((*allocation).lock).write(Spinlock::new());
        ptr::addr_of_mut!((*allocation).completion).write(completion);
        dma_fence_init(
            ptr::addr_of_mut!((*allocation).base),
            ptr::addr_of!(FENCE_OPS),
            ptr::addr_of_mut!((*allocation).lock),
            // RDIF tokens may belong to different devices/contexts/rings.
            // Without a timeline identity, never claim that a later token
            // subsumes an earlier one during sync_file/reservation dedup.
            dma_fence_context_alloc(1),
            1,
        );
    }
    allocation.cast::<DmaFence>()
}

/// Attach a display completion to the old GEM object's reservation. The
/// RDIF display token means the previously scanned-out buffer can now be
/// released; it is therefore a read-usage fence on that old object.
///
/// # Safety
/// If non-null, `obj` must point to an initialized `DrmGemObject` with a valid reference held by caller.
/// If non-null, `framebuffer` must point to an initialized `DrmFramebuffer` with a valid reference held by caller.
/// If non-null, `event_crtc` and `event` must be valid and live.
pub(crate) unsafe fn track_read_completion(
    obj: *mut DrmGemObject,
    framebuffer: *mut DrmFramebuffer,
    completion: Completion,
    event_crtc: *mut DrmCrtc,
    event: *mut DrmPendingVblankEvent,
) {
    if completion == Completion::Complete {
        // RDIF command completion releases device-use owners, but presentation
        // still completes at the next vblank on an active CRTC.
        // SAFETY: The caller owns the event and retains the live CRTC.
        unsafe { send_event(event_crtc, event) };
        return;
    }
    if !obj.is_null() {
        // SAFETY: The caller's reference protects the object while this
        // independent completion reference is acquired.
        unsafe { drm_gem_object_get(obj) };
    }
    if !framebuffer.is_null() {
        // SAFETY: Caller retains a live framebuffer ref through this call.
        unsafe { drm_framebuffer_get(framebuffer) };
    }
    let fence = new_completion_with_owners(completion, obj, framebuffer, event_crtc, event);
    if fence.is_null() {
        // The allocation-failure path waited to terminal completion and
        // released its transferred object reference before returning.
        return;
    }
    if !obj.is_null() {
        // SAFETY: 'obj' is non-null and held live by the reference acquired above. 'fence' is a valid DmaFence.
        let ret = unsafe { add_to_object_resv(obj, fence, 2) };
        if ret != 0 {
            // The pending worker still owns the object until RDIF completes;
            // do not synthesize an early error signal on reservation OOM.
            log::debug!("unable to attach GEM dma-resv display completion: {ret}");
        }
    }
    // Release the local ref; the reservation retained its own.
    // SAFETY: 'fence' is a valid DmaFence reference created by new_completion_with_owners; dropping this local reference leaves the reservation's reference intact.
    unsafe { dma_fence_put(fence) };
}

/// Add a caller-owned fence reference to a GEM reservation. The caller keeps
/// its own reference and releases it after attaching any syncobj outputs.
///
/// # Safety
/// If non-null, `obj` must point to an initialized, live `DrmGemObject` with a valid `resv` pointer.
/// If non-null, `fence` must point to an initialized, live `DmaFence`.
pub(crate) unsafe fn add_to_object_resv(
    obj: *mut DrmGemObject,
    fence: *mut DmaFence,
    usage: u32,
) -> i32 {
    if obj.is_null() || fence.is_null() {
        return -drm_kapi::config::EINVAL;
    }
    // SAFETY: 'obj' is non-null, properly aligned, and points to an initialized DrmGemObject.
    let resv = unsafe { (*obj).resv };
    if resv.is_null() {
        return -drm_kapi::config::EINVAL;
    }
    // SAFETY: The caller's GEM reference retains the reservation and lock.
    // Serialize reserve+insert: another submit must not consume the capacity
    // reserved here and turn a successful reservation into an allocation panic.
    unsafe {
        let lock = ptr::addr_of_mut!((*resv).lock);
        let ret = drm_kapi::ww_mutex::ww_mutex_lock(lock, ptr::null_mut());
        if ret != 0 {
            return ret;
        }
        let ret = dma_resv_reserve_fences(resv, 1);
        if ret == 0 {
            dma_resv_add_fence(resv, fence, usage);
        }
        drm_kapi::ww_mutex::ww_mutex_unlock(lock);
        ret
    }
}

fn schedule_completion_worker() {
    let Some(queue) = COMPLETIONS.get() else {
        return;
    };
    queue.requested.store(true, Ordering::Release);
    // SAFETY: `work` is initialized by `init`, lives in a static LazyInit, and
    // queue_work is safe to call from the notifier's short callback context.
    unsafe { queue_work(SYSTEM_DFL_WQ, queue.work.get()) };
    PUMP_WAKE.notify();
}

/// Poll every queued completion token once and return how many reached a
/// terminal state. Unconfirmed nodes go back on the shared queue; the GPU
/// lock is only held inside each status query, so fence callbacks and user
/// ioctls interleave freely with a pass.
fn poll_pending_completions() -> usize {
    let Some(queue) = COMPLETIONS.get() else {
        return 0;
    };
    let mut pending = PendingList::default();
    pending.head = queue.pending.lock().take() as usize;
    let confirmed = poll_completions(&mut pending, |completion| {
        ax_gpu::with_gpu_for_cleanup(|gpu| gpu.completion_status(completion))
            .and_then(core::convert::identity)
    });
    // Keep every unconfirmed owner pinned, without blocking unrelated
    // contexts/rings behind it. IRQ service and the pump schedule later passes.
    let mut node = pending.take();
    let mut shared = queue.pending.lock();
    while !node.is_null() {
        // SAFETY: The detached list exclusively owns these live nodes.
        let next = unsafe { (*node).next };
        shared.push(node);
        node = next;
    }
    drop(shared);
    confirmed
}

/// Whether any completion token still awaits observation. Called between
/// passes, when the shared queue owns every node.
fn completions_pending() -> bool {
    COMPLETIONS
        .get()
        .is_some_and(|queue| queue.pending.lock().head != 0)
}

/// Background completion pump (see `init`): the last-resort observer for
/// fenced RDIF completions when neither a GPU interrupt nor any user ioctl
/// re-polls the device. Mirrors the retired-fence pump of the previous
/// StarryOS card0 backend: poll fast while work is in flight, back off
/// exponentially to [`PUMP_IDLE_POLL`] when a host stalls, and park until
/// scheduled (or for the idle period) once the queue drains.
fn completion_pump() {
    let mut poll = PUMP_ACTIVE_POLL_JIFFIES;
    loop {
        let confirmed = poll_pending_completions();
        if confirmed > 0 {
            poll = PUMP_ACTIVE_POLL_JIFFIES;
        }
        if !completions_pending() {
            poll = PUMP_ACTIVE_POLL_JIFFIES;
            PUMP_WAKE.wait_for_notification(PUMP_IDLE_POLL_JIFFIES);
        } else {
            PUMP_WAKE.wait_for_notification(poll);
            poll = poll.saturating_mul(2).min(PUMP_IDLE_POLL_JIFFIES);
        }
    }
}

/// # Safety
/// Invoked by the workqueue runtime; `_work` must point to the initialized `WorkStruct` embedded in the static queue.
unsafe extern "C" fn completion_worker(_work: *mut WorkStruct) {
    let Some(queue) = COMPLETIONS.get() else {
        return;
    };
    loop {
        queue.requested.store(false, Ordering::Release);
        poll_pending_completions();
        if !queue.requested.swap(false, Ordering::AcqRel) {
            return;
        }
    }
}

/// Check each independent RDIF token once; only confirmed completion or DMA-
/// stopping device loss authorizes dropping the queue's resource owners.
/// Returns how many tokens the pass confirmed terminal.
fn poll_completions(
    pending: &mut PendingList,
    mut status: impl FnMut(Completion) -> Result<CompletionStatus, GpuError>,
) -> usize {
    let mut confirmed = 0;
    let mut node = pending.take();
    while !node.is_null() {
        // SAFETY: This detached list exclusively owns each node and its fence.
        let (next, fence, completion) = unsafe {
            let fence = (*node).fence;
            ((*node).next, fence, (*fence.cast::<RdifFence>()).completion)
        };
        let terminal = match status(completion) {
            Ok(CompletionStatus::Complete) => Some(false),
            Err(GpuError::DeviceLost) => Some(true),
            Ok(CompletionStatus::Pending) | Err(_) => None,
        };
        if let Some(failed) = terminal {
            confirmed += 1;
            // SAFETY: The terminal device state permits consuming all owners;
            // no GPU/pending-list lock is held across callbacks or final puts.
            unsafe {
                let record = node.read();
                kfree(node.cast::<c_void>());
                if failed {
                    dma_fence_set_error(fence, NEG_EIO);
                }
                dma_fence_signal(fence);
                send_event(record.event_crtc, record.event);
                drm_gem_object_put(record.keepalive_object);
                drm_framebuffer_put(record.keepalive_framebuffer);
                dma_fence_put(fence);
            }
        } else {
            pending.push(node);
        }
        node = next;
    }
    confirmed
}

fn wait_for_completion(completion: Completion) -> Result<(), GpuError> {
    let Completion::Pending(_) = completion else {
        return Ok(());
    };
    let mut last_retry_error = None;
    loop {
        match ax_gpu::wait_completion(completion, ax_gpu::GPU_WAIT_TIMEOUT) {
            Ok(()) => return Ok(()),
            Err(GpuError::TimedOut) => continue,
            // RDIF explicitly guarantees that DeviceLost stops all device
            // access. Other query/transport errors are not such a guarantee;
            // keep the GEM pin and event pending rather than report completion
            // while hardware might still reference the backing.
            Err(error @ GpuError::DeviceLost) => return Err(error),
            Err(error) => {
                if last_retry_error != Some(error) {
                    log::debug!("retrying RDIF completion wait after {error}");
                    last_retry_error = Some(error);
                }
                if drm_kapi::task_runtime::sleep_ns(1_000_000).is_err() {
                    drm_kapi::task_runtime::yield_now();
                }
            }
        }
    }
}

/// # Safety
/// Callback invoked from C `DmaFenceOps` table. Returns a static string slice valid for program duration.
unsafe extern "C" fn driver_name(_fence: *mut DmaFence) -> *const c_char {
    c"virtio_gpu".as_ptr()
}

/// # Safety
/// Callback invoked from C `DmaFenceOps` table. Returns a static string slice valid for program duration.
unsafe extern "C" fn timeline_name(_fence: *mut DmaFence) -> *const c_char {
    c"virtio-gpu".as_ptr()
}

/// # Safety
/// Callback invoked from C `DmaFenceOps` table when signaling is enabled on `_fence`.
unsafe extern "C" fn enable_signaling(_fence: *mut DmaFence) -> bool {
    schedule_completion_worker();
    true
}

/// # Safety
/// Invoked by dma-fence core when the reference count drops to 0.
/// If non-null, `fence` must point to the embedded `base` field of an `RdifFence` allocated via `kmalloc`.
unsafe extern "C" fn release_fence(fence: *mut DmaFence) {
    if fence.is_null() {
        return;
    }
    // SAFETY: base is the first field of RdifFence; dma-fence calls release
    // exactly once after its final reference has been dropped.
    unsafe {
        kfree(
            fence
                .cast::<u8>()
                .sub(offset_of!(RdifFence, base))
                .cast::<c_void>(),
        )
    };
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use drm_kapi::dma_fence::dma_fence_is_signaled;

    use super::*;

    #[test]
    fn independent_completions_do_not_block_or_release_unconfirmed_owners() {
        crate::backend::presentation::tests::init_host_services();
        let mut pending = PendingList::default();
        // SAFETY: Stack GEM shells retain their original references throughout
        // the test; the production queue takes and releases only extra pins.
        unsafe {
            let mut objects: [DrmGemObject; 3] = core::mem::zeroed();
            let mut fences = [ptr::null_mut(); 3];
            for (index, object) in objects.iter_mut().enumerate() {
                drm_kapi::memory::kref_init(ptr::addr_of_mut!(object.refcount));
                drm_gem_object_get(object);
                let fence = allocate_fence(Completion::Pending(
                    NonZeroU64::new(index as u64 + 1).unwrap(),
                ));
                assert!(!fence.is_null());
                fences[index] = dma_fence_get(fence);
                let node = kmalloc(size_of::<PendingFence>(), GFP_KERNEL).cast::<PendingFence>();
                assert!(!node.is_null());
                node.write(PendingFence {
                    fence,
                    next: ptr::null_mut(),
                    keepalive_object: object,
                    keepalive_framebuffer: ptr::null_mut(),
                    event_crtc: ptr::null_mut(),
                    event: ptr::null_mut(),
                });
                pending.push(node);
            }
            assert_ne!(
                (*fences[0]).context,
                (*fences[1]).context,
                "independent RDIF tokens must not be deduplicated as one timeline"
            );
            poll_completions(&mut pending, |completion| match completion {
                Completion::Pending(id) if id.get() == 1 => Ok(CompletionStatus::Pending),
                Completion::Pending(id) if id.get() == 2 => Ok(CompletionStatus::Complete),
                _ => Err(GpuError::Busy),
            });
            assert!(!dma_fence_is_signaled(fences[0]));
            assert!(dma_fence_is_signaled(fences[1]));
            assert!(!dma_fence_is_signaled(fences[2]));
            for (object, expected) in objects.iter().zip([2, 1, 2]) {
                assert_eq!(drm_kapi::memory::kref_read(&object.refcount), expected);
            }
            // A transient query failure did not authorize freeing backing.
            // DeviceLost is the RDIF guarantee that all DMA has stopped.
            poll_completions(&mut pending, |_| Err(GpuError::DeviceLost));
            assert_eq!(pending.head, 0);
            for (index, fence) in fences.into_iter().enumerate() {
                assert!(dma_fence_is_signaled(fence));
                assert_eq!((*fence).error, if index == 1 { 0 } else { NEG_EIO });
                assert_eq!(drm_kapi::memory::kref_read(&objects[index].refcount), 1);
                dma_fence_put(fence);
            }
        }
    }

    /// Test-body replacement for `completion_worker`: never queued, only its
    /// address is recorded by `init_work_c`.
    unsafe extern "C" fn noop_work(_work: *mut WorkStruct) {}

    #[test]
    fn fence_init_propagates_pump_spawn_failure_and_stays_retryable() {
        crate::backend::presentation::tests::init_host_services();
        // Without a task runtime provider the pump spawn fails, and the
        // failure must propagate without publishing the completion queue:
        // the spawn runs before the queue exists, so a retry after the host
        // gains a provider still performs the full sequence.
        assert!(init_with(Box::new(|| {}), noop_work).is_err());
        assert!(!COMPLETIONS.is_inited());

        // A runtime stub that accepts the spawn without starting the thread:
        // the retry succeeds, publishes the queue, and stays idempotent.
        static SPAWN_ACCEPTING_RUNTIME: drm_kapi::task_runtime::TaskRuntimeOps =
            drm_kapi::task_runtime::TaskRuntimeOps {
                current_task_id: || None,
                current_need_resched: || false,
                can_block_current: || false,
                yield_now: || {},
                sleep_ns: |_| Ok(()),
                spawn_with_policy: |_, _, _| Ok(()),
                cpu_count: || 1,
            };
        drm_kapi::task_runtime::install_task_runtime_ops(&SPAWN_ACCEPTING_RUNTIME)
            .expect("task runtime provider slot was still free");
        assert!(init_with(Box::new(|| {}), noop_work).is_ok());
        assert!(COMPLETIONS.is_inited());
        assert!(init_with(Box::new(|| {}), noop_work).is_ok());
    }
}
