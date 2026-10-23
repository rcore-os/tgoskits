// SPDX-License-Identifier: MIT
// Copyright 2026 The TGOSKits contributors.
//! RDIF completion tokens bridged into the shared Linux dma-fence core.

use core::{
    ffi::{c_char, c_void},
    mem::{MaybeUninit, offset_of, size_of},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
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
    types::{DmaFence, DmaFenceOps, Spinlock, WorkStruct},
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
    unsafe { init_work_c(&mut *queue.work.get(), completion_worker) };
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
}

/// # Safety
/// Invoked by the workqueue runtime; `_work` must point to the initialized `WorkStruct` embedded in the static queue.
unsafe extern "C" fn completion_worker(_work: *mut WorkStruct) {
    let Some(queue) = COMPLETIONS.get() else {
        return;
    };
    loop {
        queue.requested.store(false, Ordering::Release);
        let mut pending = PendingList::default();
        pending.head = queue.pending.lock().take() as usize;
        poll_completions(&mut pending, |completion| {
            ax_gpu::with_gpu_for_cleanup(|gpu| gpu.completion_status(completion))
                .and_then(core::convert::identity)
        });
        // Keep every unconfirmed owner pinned, without blocking unrelated
        // contexts/rings behind it. IRQ service schedules the next pass.
        let mut node = pending.take();
        let mut shared = queue.pending.lock();
        while !node.is_null() {
            // SAFETY: The detached list exclusively owns these live nodes.
            let next = unsafe { (*node).next };
            shared.push(node);
            node = next;
        }
        drop(shared);
        if !queue.requested.swap(false, Ordering::AcqRel) {
            return;
        }
    }
}

/// Check each independent RDIF token once; only confirmed completion or DMA-
/// stopping device loss authorizes dropping the queue's resource owners.
fn poll_completions(
    pending: &mut PendingList,
    mut status: impl FnMut(Completion) -> Result<CompletionStatus, GpuError>,
) {
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
}
