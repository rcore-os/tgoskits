// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.
//! RDIF completion tokens bridged into the shared Linux dma-fence core.

use core::{
    ffi::{c_char, c_void},
    mem::{MaybeUninit, offset_of, size_of},
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use ax_gpu::rdif_gpu::{Completion, GpuError};
use ax_lazyinit::LazyInit;
use drm_kapi::{
    config::GFP_KERNEL,
    dma_fence::{
        dma_fence_context_alloc, dma_fence_get, dma_fence_init, dma_fence_put, dma_fence_set_error,
        dma_fence_signal,
    },
    dma_resv::{dma_resv_add_fence, dma_resv_reserve_fences},
    locks::{spin_lock_irq, spin_unlock_irq},
    memory::{kfree, kmalloc},
    types::{DmaFence, DmaFenceOps, Spinlock, WorkStruct},
    workqueue::{INIT_WORK_C, queue_work, system_dfl_wq},
};
use kdrm_core::{
    drm_crtc_types::DrmCrtc,
    drm_framebuffer_types::DrmFramebuffer,
    drm_framebuffer_upstream::{drm_framebuffer_get, drm_framebuffer_put},
    drm_gem_types::DrmGemObject,
    drm_gem_upstream::{drm_gem_object_get, drm_gem_object_put},
    drm_vblank_types::DrmPendingVblankEvent,
    drm_vblank_upstream::drm_crtc_send_vblank_event,
};

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
        // SAFETY: WorkStruct is a C work-item record whose INIT_WORK_C
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
static NEXT_SEQNO: AtomicU64 = AtomicU64::new(1);
static FENCE_CONTEXT: AtomicU64 = AtomicU64::new(0);

static FENCE_OPS: DmaFenceOps = DmaFenceOps {
    get_driver_name: Some(driver_name),
    get_timeline_name: Some(timeline_name),
    enable_signaling: Some(enable_signaling),
    signaled: None,
    wait: None,
    release: Some(release_fence),
    set_deadline: None,
};

pub(crate) fn init() {
    if COMPLETIONS.is_inited() {
        return;
    }
    COMPLETIONS.init_once(CompletionQueue::new());
    let queue = COMPLETIONS.get().expect("completion queue was initialized");
    // SAFETY: The LazyInit static gives the work record a stable address for
    // the queue's entire lifetime. The runtime workqueue provider is installed
    // before this function is called by ax-drm initialization.
    unsafe { INIT_WORK_C(&mut *queue.work.get(), completion_worker) };
    FENCE_CONTEXT.store(dma_fence_context_alloc(1), Ordering::Release);
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
    let allocation = kmalloc(size_of::<RdifFence>(), GFP_KERNEL).cast::<RdifFence>();
    if allocation.is_null() {
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
    // SAFETY: The KAPI allocation has the required size/alignment. Initialize
    // the external lock and immutable completion token before dma_fence_init.
    unsafe {
        ptr::addr_of_mut!((*allocation).lock).write(Spinlock::new());
        ptr::addr_of_mut!((*allocation).completion).write(completion);
        dma_fence_init(
            ptr::addr_of_mut!((*allocation).base),
            ptr::addr_of!(FENCE_OPS),
            ptr::addr_of_mut!((*allocation).lock),
            FENCE_CONTEXT.load(Ordering::Acquire),
            NEXT_SEQNO.fetch_add(1, Ordering::Relaxed),
        );
    }
    let fence = allocation.cast::<DmaFence>();
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
        // SAFETY: Complete means no device use remains; sending the atomic
        // event now is the same completion edge as the RDIF result.
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
    // SAFETY: 'resv' is non-null and points to a valid DmaResv reservation object.
    let ret = unsafe { dma_resv_reserve_fences(resv, 1) };
    if ret != 0 {
        return ret;
    }
    // SAFETY: 'resv' is valid and has sufficient fence slots reserved above; 'fence' is non-null and live.
    unsafe { dma_resv_add_fence(resv, fence, usage) };
    0
}

fn schedule_completion_worker() {
    let Some(queue) = COMPLETIONS.get() else {
        return;
    };
    queue.requested.store(true, Ordering::Release);
    // SAFETY: `work` is initialized by `init`, lives in a static LazyInit, and
    // queue_work is safe to call from the notifier's short callback context.
    unsafe { queue_work(system_dfl_wq, &mut *queue.work.get()) };
}

/// # Safety
/// Invoked by the workqueue runtime; `_work` must point to the initialized `WorkStruct` embedded in the static queue.
unsafe extern "C" fn completion_worker(_work: *mut WorkStruct) {
    let Some(queue) = COMPLETIONS.get() else {
        return;
    };
    loop {
        queue.requested.store(false, Ordering::Release);
        let mut node = queue.pending.lock().take();
        while !node.is_null() {
            // SAFETY: 'node' was popped from queue.pending, is non-null, properly aligned, and points to an initialized PendingFence.
            let record = unsafe { node.read() };
            // SAFETY: 'node' was allocated with kmalloc and has been fully read into 'record'; deallocating it here prevents leaks.
            unsafe { kfree(node.cast::<c_void>()) };
            node = record.next;
            let fence = record.fence;
            // SAFETY: Each queued item owns a dma_fence reference; the C base
            // is the first field of RdifFence, so this recovers its token.
            let completion = unsafe {
                (*fence
                    .cast::<u8>()
                    .sub(offset_of!(RdifFence, base))
                    .cast::<RdifFence>())
                .completion
            };
            let result = wait_for_completion(completion);
            if result.is_err() {
                // DeviceLost is terminal for every token. Other terminal
                // transport errors likewise cannot be reported as success.
                // SAFETY: 'fence' is a valid DmaFence owned by this completion worker.
                unsafe { dma_fence_set_error(fence, NEG_EIO) };
            }
            // `wait_completion` returned after leaving the ax-gpu lock, so
            // dma_fence callbacks may safely wake sync_file/poll consumers.
            // SAFETY: 'fence' is a valid live DmaFence; all keepalive references and events are consumed here.
            unsafe {
                dma_fence_signal(fence);
                send_event(record.event_crtc, record.event);
                drm_gem_object_put(record.keepalive_object);
                drm_framebuffer_put(record.keepalive_framebuffer);
                dma_fence_put(fence);
            }
        }
        if !queue.requested.swap(false, Ordering::AcqRel) {
            return;
        }
    }
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
/// If non-null, `crtc` and `event` must point to live, properly initialized `DrmCrtc` and `DrmPendingVblankEvent`.
unsafe fn send_event(crtc: *mut DrmCrtc, event: *mut DrmPendingVblankEvent) {
    if crtc.is_null() || event.is_null() {
        return;
    }
    // drm_crtc_send_vblank_event expects event_lock held, as in the upstream
    // simple-KMS atomic_flush helper. This runs after wait_completion has left
    // the GPU lock, so event/fence callbacks cannot re-enter ax-gpu under it.
    // SAFETY: 'crtc' and 'event' are non-null and valid. (*crtc).dev is checked against null before accessing event_lock.
    unsafe {
        let dev = (*crtc).dev;
        if dev.is_null() {
            return;
        }
        spin_lock_irq(&mut (*dev).event_lock);
        drm_crtc_send_vblank_event(crtc, event);
        spin_unlock_irq(&mut (*dev).event_lock);
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
