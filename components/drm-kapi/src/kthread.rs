// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Persistent Linux kthread workers for serialized kernel work.
//!
//! The worker queue is intrusive and IRQ-safe: queueing a C-shaped
//! `kthread_work` allocates nothing, and callbacks execute only in a detached
//! task created with the requested scheduler policy. Work ownership remains
//! with its caller until it is flushed or synchronously cancelled.

use alloc::{boxed::Box, string::String};
use core::{
    ffi::{c_int, c_ulong},
    mem::{offset_of, size_of},
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
};

use crate::{
    config::{EAGAIN, ENODEV, ENOMEM},
    list::{init_list_head, list_add_tail, list_del_init, list_empty},
    locks::{spin_lock_irqsave_raw, spin_unlock_irqrestore_raw},
    memory::{kfree, kmalloc},
    task_runtime::{self, TaskEntry, TaskSchedulingPolicy},
    types::{ListHead, Spinlock},
    wait::{self, wait_timeout_until},
};

/// The callback stored in a Linux `struct kthread_work`.
pub type KthreadWorkFn = unsafe extern "C" fn(*mut KthreadWork);

/// C-layout Linux `struct kthread_work` used by DRM vblank workers.
#[repr(C)]
pub struct KthreadWork {
    node: ListHead,
    func: Option<KthreadWorkFn>,
    worker: *mut KthreadWorker,
    canceling: c_int,
}

const _: [(); 40] = [(); size_of::<KthreadWork>()];
const _: [(); 0] = [(); offset_of!(KthreadWork, node)];
const _: [(); 16] = [(); offset_of!(KthreadWork, func)];
const _: [(); 24] = [(); offset_of!(KthreadWork, worker)];
const _: [(); 32] = [(); offset_of!(KthreadWork, canceling)];

/// Opaque worker handle. Its Rust state is private; only this module accesses
/// queue metadata, and every such access is protected by `KTHREAD_LOCK`.
pub struct KthreadWorker {
    work_list: ListHead,
    running: *mut KthreadWork,
    stopping: bool,
    start_status: AtomicI32,
    terminated: AtomicBool,
}

// SAFETY: pointers in the intrusive list are accessed only under KTHREAD_LOCK;
// the worker task is the sole callback executor for this worker.
unsafe impl Send for KthreadWorker {}
// SAFETY: shared state is synchronized by KTHREAD_LOCK and startup/termination
// publication uses atomics.
unsafe impl Sync for KthreadWorker {}

impl KthreadWorker {
    fn new() -> Self {
        Self {
            work_list: ListHead {
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
            },
            running: ptr::null_mut(),
            stopping: false,
            start_status: AtomicI32::new(0),
            terminated: AtomicBool::new(false),
        }
    }
}

// One short IRQ-saving lock serializes each worker's intrusive queue and the
// `work.worker` ownership word. Queueing from hard-IRQ cannot allocate or sleep.
static mut KTHREAD_LOCK: Spinlock = Spinlock::new();
static CHANGE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// # Safety
/// Caller must ensure that the lock is released with `unlock_state` using the returned flags,
/// avoiding deadlocks or unbalanced interrupt flags.
#[inline]
unsafe fn lock_state() -> c_ulong {
    let mut flags: c_ulong = 0;
    // SAFETY: KTHREAD_LOCK is static, initialized unlocked, and all accesses to
    // queue and work ownership metadata use this same raw lock.
    unsafe {
        spin_lock_irqsave_raw(ptr::addr_of_mut!(KTHREAD_LOCK), &mut flags);
    }
    flags
}

/// # Safety
/// `flags` must have been returned by a previous matching `lock_state` call.
#[inline]
unsafe fn unlock_state(flags: c_ulong) {
    // SAFETY: `flags` came from the matching lock_state() acquisition.
    unsafe { spin_unlock_irqrestore_raw(ptr::addr_of_mut!(KTHREAD_LOCK), flags) };
}

#[inline]
fn signal_change() {
    CHANGE_GENERATION.fetch_add(1, Ordering::Release);
    // The hosting WaitOps wake callback must be safe from the contexts that
    // queue kthread work, including hard IRQs.
    wait::wake_all();
}

/// # Safety
/// `node` must be a valid pointer to the `node` field of an active `KthreadWork`.
#[inline]
unsafe fn work_from_node(node: *mut ListHead) -> *mut KthreadWork {
    // SAFETY: every node linked on a worker list is KthreadWork::node.
    unsafe {
        node.cast::<u8>()
            .sub(offset_of!(KthreadWork, node))
            .cast::<KthreadWork>()
    }
}

/// Initialize a work item before publishing it to a worker.
///
/// # Safety
/// `work` must be a writable, aligned `KthreadWork` which is not queued or
/// running. Its storage must remain live until it is flushed/cancelled.
pub unsafe fn init_work(work: *mut KthreadWork, func: KthreadWorkFn) {
    assert!(!work.is_null());
    // SAFETY: Caller guarantees `work` is non-null, aligned, writable, and not queued or running.
    unsafe {
        init_list_head(ptr::addr_of_mut!((*work).node));
        (*work).func = Some(func);
        (*work).worker = ptr::null_mut();
        (*work).canceling = 0;
    }
}

/// Create a serialized worker task using Linux's SCHED_FIFO policy.
///
/// The scheduler provider must implement FIFO or return `-EOPNOTSUPP`; KAPI
/// never silently degrades a requested FIFO worker to normal priority.
pub fn run_worker(name: String) -> Result<*mut KthreadWorker, c_int> {
    run_worker_with_policy(name, TaskSchedulingPolicy::Fifo)
}

fn run_worker_with_policy(
    name: String,
    policy: TaskSchedulingPolicy,
) -> Result<*mut KthreadWorker, c_int> {
    if !wait::wait_ops_installed() {
        return Err(-ENODEV);
    }
    if !task_runtime::can_block_current() {
        return Err(-EAGAIN);
    }

    let worker =
        kmalloc(size_of::<KthreadWorker>(), crate::config::GFP_KERNEL).cast::<KthreadWorker>();
    if worker.is_null() {
        return Err(-ENOMEM);
    }
    // SAFETY: `worker` was allocated via `kmalloc` above with capacity for `KthreadWorker`.
    unsafe {
        ptr::write(worker, KthreadWorker::new());
        init_list_head(ptr::addr_of_mut!((*worker).work_list));
    }

    let worker_addr = worker as usize;
    let entry: TaskEntry = Box::new(move || worker_main(worker_addr));
    if let Err(error) = task_runtime::try_spawn_with_policy(name, policy, entry) {
        // SAFETY: `worker` was allocated via `kmalloc` above and spawning failed.
        unsafe { kfree(worker) };
        return Err(error);
    }

    // Do not publish a handle until the spawned task confirms it has a real
    // blockable context and can use the already-installed wait provider.
    loop {
        // SAFETY: `worker` is valid and remains allocated during startup synchronization.
        let status = unsafe { (*worker).start_status.load(Ordering::Acquire) };
        if status != 0 {
            if status < 0 {
                // SAFETY: `worker` startup failed, freeing allocated memory.
                unsafe { kfree(worker) };
                return Err(status);
            }
            return Ok(worker);
        }
        // SAFETY: `worker` is valid during the wait predicate.
        if let Err(error) = wait::try_wait_until_ready(None, &|| unsafe {
            (*worker).start_status.load(Ordering::Acquire) != 0
        }) {
            request_stop(worker);
            wait_for_termination_uninterruptible(worker);
            // SAFETY: Worker was stopped and confirmed terminated before freeing.
            unsafe { kfree(worker) };
            return Err(error);
        }
    }
}

fn worker_main(worker_addr: usize) {
    let worker = worker_addr as *mut KthreadWorker;
    if !task_runtime::can_block_current() || !wait::wait_ops_installed() {
        // SAFETY: `worker` points to the `KthreadWorker` allocated for this task.
        unsafe { (*worker).start_status.store(-EAGAIN, Ordering::Release) };
        // SAFETY: `worker` points to the `KthreadWorker` allocated for this task.
        unsafe { (*worker).terminated.store(true, Ordering::Release) };
        signal_change();
        return;
    }
    // SAFETY: `worker` points to the `KthreadWorker` allocated for this task.
    unsafe { (*worker).start_status.store(1, Ordering::Release) };
    signal_change();

    loop {
        // SAFETY: Acquires `KTHREAD_LOCK` to inspect worker queue.
        let flags = unsafe { lock_state() };
        // SAFETY: Accesses `worker->work_list` under `KTHREAD_LOCK`.
        if !unsafe { list_empty(&(*worker).work_list) } {
            // SAFETY: `worker->work_list` is non-empty under `KTHREAD_LOCK`.
            let node = unsafe { (*worker).work_list.next };
            // SAFETY: `node` is linked on `worker->work_list` and belongs to a `KthreadWork`.
            let work = unsafe { work_from_node(node) };
            // SAFETY: Under `KTHREAD_LOCK`, removes `node` from queue and sets `worker->running`.
            unsafe {
                list_del_init(node);
                (*worker).running = work;
            }
            // SAFETY: Releases `KTHREAD_LOCK` before invoking the work callback.
            unsafe { unlock_state(flags) };

            // SAFETY: `work` was retrieved from `worker->work_list` and remains valid.
            let callback = unsafe { (*work).func };
            if let Some(callback) = callback {
                // SAFETY: `work` is valid and caller of `init_work` guaranteed the callback is safe to invoke with `work`.
                unsafe { callback(work) };
            }

            // SAFETY: Re-acquires `KTHREAD_LOCK` to update worker state.
            let flags = unsafe { lock_state() };
            // SAFETY: Updates `worker->running` and `work->worker` under `KTHREAD_LOCK`.
            unsafe {
                (*worker).running = ptr::null_mut();
                if (*work).worker == worker && list_empty(&(*work).node) {
                    (*work).worker = ptr::null_mut();
                }
            }
            CHANGE_GENERATION.fetch_add(1, Ordering::Release);
            // SAFETY: Releases `KTHREAD_LOCK`.
            unsafe { unlock_state(flags) };
            wait::wake_all();
            continue;
        }

        // SAFETY: Reads `stopping` under `KTHREAD_LOCK`.
        if unsafe { (*worker).stopping } {
            // SAFETY: Releases `KTHREAD_LOCK` before loop break.
            unsafe { unlock_state(flags) };
            break;
        }
        let observed = CHANGE_GENERATION.load(Ordering::Acquire);
        // SAFETY: Releases `KTHREAD_LOCK` before wait.
        unsafe { unlock_state(flags) };
        let _ = wait_timeout_until(None, &|| {
            if CHANGE_GENERATION.load(Ordering::Acquire) != observed {
                return true;
            }
            worker_has_work_or_stopping(worker)
        });
    }

    // SAFETY: Updates terminated atomic flag on worker task exit.
    unsafe { (*worker).terminated.store(true, Ordering::Release) };
    signal_change();
}

fn worker_has_work_or_stopping(worker: *mut KthreadWorker) -> bool {
    // SAFETY: Acquires `KTHREAD_LOCK` to inspect worker state.
    let flags = unsafe { lock_state() };
    // SAFETY: Reads `work_list` and `stopping` under `KTHREAD_LOCK`.
    let ready = unsafe { !list_empty(&(*worker).work_list) || (*worker).stopping };
    // SAFETY: Releases `KTHREAD_LOCK`.
    unsafe { unlock_state(flags) };
    ready
}

/// Queue an item for later execution by a live worker.
///
/// # Safety
/// Both pointers must refer to live objects created/initialized by this KAPI
/// until the item has completed or been synchronously cancelled.
pub unsafe fn queue_work(worker: *mut KthreadWorker, work: *mut KthreadWork) -> bool {
    if worker.is_null() || work.is_null() {
        return false;
    }

    // SAFETY: Acquires `KTHREAD_LOCK` to safely validate and enqueue `work`.
    let flags = unsafe { lock_state() };
    // SAFETY: Accesses `work->node` under `KTHREAD_LOCK`.
    let already_queued = unsafe { !list_empty(&(*work).node) };
    // SAFETY: Accesses `work->worker` under `KTHREAD_LOCK`.
    let belongs_elsewhere = unsafe { !(*work).worker.is_null() && (*work).worker != worker };
    // SAFETY: Accesses worker and work state fields under `KTHREAD_LOCK`.
    let invalid = unsafe {
        (*worker).stopping
            || (*worker).terminated.load(Ordering::Acquire)
            || (*work).canceling != 0
            || (*work).func.is_none()
    };
    if already_queued || belongs_elsewhere || invalid {
        // SAFETY: Releases `KTHREAD_LOCK` on error exit.
        unsafe { unlock_state(flags) };
        return false;
    }

    // SAFETY: Sets `work->worker` and appends `work->node` to `worker->work_list` under `KTHREAD_LOCK`.
    unsafe {
        (*work).worker = worker;
        list_add_tail(
            ptr::addr_of_mut!((*work).node),
            ptr::addr_of_mut!((*worker).work_list),
        );
    }
    CHANGE_GENERATION.fetch_add(1, Ordering::Release);
    // SAFETY: Releases `KTHREAD_LOCK`.
    unsafe { unlock_state(flags) };
    wait::wake_all();
    true
}

fn work_active(work: *mut KthreadWork) -> bool {
    // SAFETY: Acquires `KTHREAD_LOCK`.
    let flags = unsafe { lock_state() };
    // SAFETY: Reads `work->node` and `work->worker` under `KTHREAD_LOCK`.
    let active = unsafe { !list_empty(&(*work).node) || !(*work).worker.is_null() };
    // SAFETY: Releases `KTHREAD_LOCK`.
    unsafe { unlock_state(flags) };
    active
}

fn worker_active(worker: *mut KthreadWorker) -> bool {
    // SAFETY: Acquires `KTHREAD_LOCK`.
    let flags = unsafe { lock_state() };
    // SAFETY: Reads `worker->work_list` and `worker->running` under `KTHREAD_LOCK`.
    let active = unsafe { !list_empty(&(*worker).work_list) || !(*worker).running.is_null() };
    // SAFETY: Releases `KTHREAD_LOCK`.
    unsafe { unlock_state(flags) };
    active
}

fn wait_for(ready: impl Fn() -> bool) {
    assert!(
        task_runtime::can_block_current() && wait::wait_ops_installed(),
        "synchronous kthread work operation requires task and wait providers"
    );
    while !ready() {
        let _ = wait::try_wait_until_ready(None, &ready)
            .expect("kthread work wait provider became unavailable");
    }
}

/// Remove queued work and wait for any running callback to complete.
///
/// Returns true when the item was queued or running at entry.
///
/// # Safety
/// `work` must remain live and initialized until this function returns.
pub unsafe fn cancel_work_sync(work: *mut KthreadWork) -> bool {
    if work.is_null() {
        return false;
    }

    // SAFETY: Acquires `KTHREAD_LOCK` to cancel work safely.
    let flags = unsafe { lock_state() };
    // SAFETY: Increments `canceling` under `KTHREAD_LOCK`.
    unsafe { (*work).canceling = (*work).canceling.saturating_add(1) };
    // SAFETY: Reads `work->worker` under `KTHREAD_LOCK`.
    let owner = unsafe { (*work).worker };
    // SAFETY: Checks whether work is in a queue under `KTHREAD_LOCK`.
    let was_queued = unsafe { !list_empty(&(*work).node) };
    if was_queued {
        // SAFETY: Deletes `work->node` from its queue under `KTHREAD_LOCK`.
        unsafe { list_del_init(ptr::addr_of_mut!((*work).node)) };
    }
    // SAFETY: Reads `owner->running` under `KTHREAD_LOCK`.
    let running = !owner.is_null() && unsafe { (*owner).running == work };
    if !running {
        // SAFETY: Clears `work->worker` under `KTHREAD_LOCK` since it is not currently executing.
        unsafe { (*work).worker = ptr::null_mut() };
    }
    let was_active = was_queued || running;
    CHANGE_GENERATION.fetch_add(1, Ordering::Release);
    // SAFETY: Releases `KTHREAD_LOCK`.
    unsafe { unlock_state(flags) };
    wait::wake_all();

    if running {
        wait_for(|| !work_active(work));
    }

    // SAFETY: Re-acquires `KTHREAD_LOCK` to decrement canceling count.
    let flags = unsafe { lock_state() };
    // SAFETY: Decrements `work->canceling` under `KTHREAD_LOCK`.
    unsafe { (*work).canceling -= 1 };
    CHANGE_GENERATION.fetch_add(1, Ordering::Release);
    // SAFETY: Releases `KTHREAD_LOCK`.
    unsafe { unlock_state(flags) };
    wait::wake_all();
    was_active
}

/// Wait until the item is neither queued nor executing.
///
/// # Safety
/// `work` must remain live and initialized until this function returns.
pub unsafe fn flush_work(work: *mut KthreadWork) {
    if work.is_null() {
        return;
    }
    wait_for(|| !work_active(work));
}

/// Wait until the worker has no queued or executing work.
///
/// # Safety
/// `worker` must remain live until this function returns.
pub unsafe fn flush_worker(worker: *mut KthreadWorker) {
    if worker.is_null() {
        return;
    }
    wait_for(|| !worker_active(worker));
}

/// Stop the worker after draining queued callbacks and release its handle.
///
/// # Safety
/// The caller must prevent new submissions and retain exclusive ownership of
/// the handle until destruction completes.
pub unsafe fn destroy_worker(worker: *mut KthreadWorker) {
    if worker.is_null() {
        return;
    }
    // SAFETY: Acquires `KTHREAD_LOCK` to set stopping flag.
    let flags = unsafe { lock_state() };
    // SAFETY: Sets `worker->stopping` under `KTHREAD_LOCK`.
    unsafe { (*worker).stopping = true };
    CHANGE_GENERATION.fetch_add(1, Ordering::Release);
    // SAFETY: Releases `KTHREAD_LOCK`.
    unsafe { unlock_state(flags) };
    wait::wake_all();
    // SAFETY: Reads `terminated` atomic flag on `worker`.
    wait_for(|| unsafe { (*worker).terminated.load(Ordering::Acquire) });
    // SAFETY: Caller guarantees exclusive ownership and worker thread has terminated.
    unsafe { kfree(worker) };
}

fn request_stop(worker: *mut KthreadWorker) {
    // SAFETY: Acquires `KTHREAD_LOCK`.
    let flags = unsafe { lock_state() };
    // SAFETY: Sets `worker->stopping` under `KTHREAD_LOCK`.
    unsafe { (*worker).stopping = true };
    CHANGE_GENERATION.fetch_add(1, Ordering::Release);
    // SAFETY: Releases `KTHREAD_LOCK`.
    unsafe { unlock_state(flags) };
    wait::wake_all();
}

fn wait_for_termination_uninterruptible(worker: *mut KthreadWorker) {
    // SAFETY: Reads `terminated` atomic flag until termination completes.
    while !unsafe { (*worker).terminated.load(Ordering::Acquire) } {
        task_runtime::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicPtr, AtomicUsize};

    use super::*;

    static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static REQUEUE_WORKER: AtomicPtr<KthreadWorker> = AtomicPtr::new(ptr::null_mut());
    static CANCEL_BLOCK_ENTERED: AtomicBool = AtomicBool::new(false);
    static CANCEL_BLOCK_RELEASE: AtomicBool = AtomicBool::new(false);
    static CANCELLED_CALLBACKS: AtomicUsize = AtomicUsize::new(0);

    /// # Safety
    /// Test callback invoked by kthread worker with valid `_work`.
    unsafe extern "C" fn requeue_once(_work: *mut KthreadWork) {
        if CALLBACKS.fetch_add(1, Ordering::SeqCst) == 0 {
            let worker = REQUEUE_WORKER.load(Ordering::SeqCst);
            // SAFETY: `worker` is live and `_work` was passed to this callback.
            assert!(unsafe { queue_work(worker, _work) });
        }
    }

    /// # Safety
    /// Test callback invoked by kthread worker.
    unsafe extern "C" fn block_for_cancel(_work: *mut KthreadWork) {
        CANCEL_BLOCK_ENTERED.store(true, Ordering::Release);
        while !CANCEL_BLOCK_RELEASE.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }

    /// # Safety
    /// Test callback invoked by kthread worker.
    unsafe extern "C" fn should_be_cancelled(_work: *mut KthreadWork) {
        CANCELLED_CALLBACKS.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn worker_requeues_running_work_flushes_and_cancels_queued_work() {
        let _serial = task_runtime::test_serial_runtime();
        // The crate's cfg(test) wait provider is a real host wait loop. Prime
        // it here so production-style worker creation validates the provider.
        let _ = wait::wait_timeout_until(Some(0), &|| false);
        assert!(matches!(
            run_worker("kthread-fifo-required".into()),
            Err(error) if error == -crate::config::EOPNOTSUPP
        ));

        CALLBACKS.store(0, Ordering::SeqCst);
        CANCEL_BLOCK_ENTERED.store(false, Ordering::SeqCst);
        CANCEL_BLOCK_RELEASE.store(false, Ordering::SeqCst);
        CANCELLED_CALLBACKS.store(0, Ordering::SeqCst);

        let worker =
            run_worker_with_policy("kthread-test".into(), TaskSchedulingPolicy::Normal).unwrap();
        REQUEUE_WORKER.store(worker, Ordering::SeqCst);
        let mut repeat = KthreadWork {
            node: ListHead {
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
            },
            func: None,
            worker: ptr::null_mut(),
            canceling: 0,
        };
        // SAFETY: Stack-allocated `repeat` is valid and not yet queued.
        unsafe { init_work(&mut repeat, requeue_once) };
        // SAFETY: `worker` and `repeat` are live.
        assert!(unsafe { queue_work(worker, &mut repeat) });
        // SAFETY: `repeat` remains live during flush.
        unsafe { flush_work(&mut repeat) };
        assert_eq!(CALLBACKS.load(Ordering::SeqCst), 2);

        let mut blocker = KthreadWork {
            node: ListHead {
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
            },
            func: None,
            worker: ptr::null_mut(),
            canceling: 0,
        };
        let mut cancelled = KthreadWork {
            node: ListHead {
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
            },
            func: None,
            worker: ptr::null_mut(),
            canceling: 0,
        };
        // SAFETY: `blocker` and `cancelled` are valid stack variables initialized for work.
        unsafe {
            init_work(&mut blocker, block_for_cancel);
            init_work(&mut cancelled, should_be_cancelled);
            assert!(queue_work(worker, &mut blocker));
        }
        while !CANCEL_BLOCK_ENTERED.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        // SAFETY: `worker` and `cancelled` are valid.
        assert!(unsafe { queue_work(worker, &mut cancelled) });
        // SAFETY: `cancelled` is valid and live.
        assert!(unsafe { cancel_work_sync(&mut cancelled) });
        assert_eq!(CANCELLED_CALLBACKS.load(Ordering::SeqCst), 0);
        CANCEL_BLOCK_RELEASE.store(true, Ordering::Release);
        // SAFETY: `worker` is flushed and destroyed cleanly.
        unsafe {
            flush_worker(worker);
            destroy_worker(worker);
        }
        REQUEUE_WORKER.store(ptr::null_mut(), Ordering::SeqCst);
    }
}
