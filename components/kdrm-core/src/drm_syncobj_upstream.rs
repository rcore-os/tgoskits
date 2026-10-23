// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux DRM syncobj core used by render drivers.
//!
//! Syncobjs own ordinary Linux `dma_fence` references and timeline payloads
//! are Linux `dma_fence_chain` nodes. VirtGPU and other drivers therefore use
//! the same fence/sync-file representation as the rest of the DRM core.

use alloc::{sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    ffi::{c_char, c_int, c_long, c_void},
    mem::{MaybeUninit, size_of},
    ptr,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use drm_kapi::{
    config::{EFAULT, EINVAL, ENOENT, ENOMEM, EOPNOTSUPP, ERESTARTSYS, ETIME, GFP_KERNEL},
    kernel_services::{eventfd_ctx_fdget, eventfd_ctx_put, eventfd_signal},
    memory::{kfree, kmalloc},
    mm_native::{copy_from_user, copy_to_user},
    shmem::{File, FileOperations, Inode, fput_file},
    sync_file::{sync_file_create, sync_file_get_fence},
    types::{
        DmaFence, DmaFenceCb, DmaFenceChain, ListHead, Spinlock, WaitQueueEntry, WaitQueueHead,
    },
    wait::{add_wait_queue, init_waitqueue_head, remove_wait_queue, wake_up_all},
};

use crate::{
    drm_crtc_upstream::DmaFenceOps,
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    uapi_drm_consts::{
        DRM_SYNCOBJ_CREATE_SIGNALED, DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
        DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE, DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE,
        DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE, DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED,
        DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE,
        DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
    },
    uapi_drm_types::{
        DrmSyncobjArray, DrmSyncobjCreate, DrmSyncobjDestroy, DrmSyncobjEventfd, DrmSyncobjHandle,
        DrmSyncobjTimelineArray, DrmSyncobjTimelineWait, DrmSyncobjTransfer, DrmSyncobjWait,
    },
};

const DRIVER_SYNCOBJ: u32 = 1 << 5;
const DRIVER_SYNCOBJ_TIMELINE: u32 = 1 << 6;
const MAX_SYNCOBJ_COUNT: usize = 1 << 16;
const WAIT_FOR_SUBMIT_TIMEOUT_JIFFIES: u64 = 5000;
const O_CLOEXEC: c_int = 0x80000;

/// Shared hosted file-descriptor provider for DRM crate tests. The LinuxKPI
/// provider is install-once, so all tests in this crate must use the same
/// table rather than registering independent mocks.
#[cfg(test)]
pub(crate) mod test_file_ops {
    use alloc::vec::Vec;
    use core::{
        ffi::{c_char, c_int, c_void},
        ptr,
        sync::atomic::{AtomicI32, AtomicUsize, Ordering},
    };

    use drm_kapi::{
        config::{err_ptr, is_err},
        kernel_services::{FileOps, install_file_ops},
        shmem::{File, FileOperations, fget_file, fput_file, shmem_file_setup},
    };
    use spin::{Mutex, MutexGuard};

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static FD_TABLE: Mutex<Vec<(c_int, usize)>> = Mutex::new(Vec::new());
    static NEXT_FD: AtomicI32 = AtomicI32::new(10);
    static NEXT_FD_ERROR: AtomicI32 = AtomicI32::new(0);
    static ANON_INODE_ERROR: AtomicI32 = AtomicI32::new(0);
    static PUT_UNUSED_COUNT: AtomicUsize = AtomicUsize::new(0);

    static PLAIN_FILE_OPS: FileOperations = FileOperations::empty();

    /// Mock implementation of Linux `anon_inode_getfile()` for hosted DRM tests.
    ///
    /// # Safety
    /// - `name` must be null or point to a valid null-terminated C string.
    /// - `fops` must point to a live `FileOperations` structure outliving the returned file.
    /// - `private_data` must be a valid pointer or cookie for the file instance.
    unsafe extern "C" fn anon_inode_getfile(
        name: *const c_char,
        fops: *const c_void,
        private_data: *mut c_void,
        flags: c_int,
    ) -> *mut File {
        let error = ANON_INODE_ERROR.swap(0, Ordering::SeqCst);
        if error != 0 {
            return err_ptr::<File>(error);
        }
        // SAFETY: `name` is null or points to a valid null-terminated string; shmem_file_setup initializes and returns a File or error pointer.
        let file = unsafe { shmem_file_setup(name, 0, 0) };
        if file.is_null() || is_err(file) {
            return file.cast();
        }
        // SAFETY: `file` is confirmed non-null and not an error pointer; we have exclusive initialization access to this newly created File.
        unsafe {
            (*file).f_op = fops.cast();
            (*file)._private_data = private_data;
            (*file).f_flags = flags as u32;
        }
        file
    }

    /// # Safety
    /// Callable via C ABI function pointer from test file-ops provider. `_flags` is passed by value.
    unsafe extern "C" fn get_unused_fd_flags(_flags: c_int) -> c_int {
        let error = NEXT_FD_ERROR.swap(0, Ordering::SeqCst);
        if error != 0 {
            error
        } else {
            NEXT_FD.fetch_add(1, Ordering::SeqCst)
        }
    }

    /// # Safety
    /// Callable via C ABI function pointer from test file-ops provider. `_fd` is passed by value.
    unsafe extern "C" fn put_unused_fd(_fd: c_int) {
        PUT_UNUSED_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    /// # Safety
    /// `file` must be null or point to a valid `File` structure allocated by shmem or the test harness.
    unsafe extern "C" fn fd_install(fd: c_int, file: *mut File) {
        FD_TABLE.lock().push((fd, file as usize));
    }

    /// # Safety
    /// Callable via C ABI function pointer. If `fd` is present in `FD_TABLE`, its stored pointer must remain a valid `File`.
    unsafe extern "C" fn fget(fd: c_int) -> *mut File {
        let table = FD_TABLE.lock();
        match table.iter().find(|(entry, _)| *entry == fd) {
            // SAFETY: `*file` was installed into FD_TABLE as a live `File` pointer; fget_file increments its reference count.
            Some((_, file)) => unsafe { fget_file(*file as *mut File) },
            None => ptr::null_mut(),
        }
    }

    static TEST_FILE_OPS: FileOps = FileOps {
        anon_inode_getfile,
        get_unused_fd_flags,
        put_unused_fd,
        fd_install,
        fget,
    };

    pub(crate) fn install() {
        install_file_ops(&TEST_FILE_OPS)
            .expect("DRM tests must share one LinuxKPI file-ops provider");
    }

    pub(crate) fn serial() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock()
    }

    pub(crate) fn fail_next_fd(error: c_int) {
        NEXT_FD_ERROR.store(error, Ordering::SeqCst);
    }

    pub(crate) fn fail_next_anon_inode(error: c_int) {
        ANON_INODE_ERROR.store(error, Ordering::SeqCst);
    }

    pub(crate) fn put_unused_count() -> usize {
        PUT_UNUSED_COUNT.load(Ordering::SeqCst)
    }

    pub(crate) fn install_plain_file() -> c_int {
        // SAFETY: get_unused_fd_flags takes an integer flag by value and allocates a test descriptor.
        let fd = unsafe { get_unused_fd_flags(0) };
        assert!(fd >= 0);
        // SAFETY: The literal c-string pointer is null-terminated, aligned, and valid for reads.
        let file = unsafe { shmem_file_setup(c"plain-test-file".as_ptr(), 0, 0) };
        assert!(!file.is_null() && !is_err(file));
        // SAFETY: `file` is confirmed non-null and not an error pointer; PLAIN_FILE_OPS is static and lives for the entire test process.
        unsafe {
            (*file).f_op = ptr::addr_of!(PLAIN_FILE_OPS);
            fd_install(fd, file.cast());
        }
        fd
    }

    pub(crate) fn close_fd(fd: c_int) {
        let mut table = FD_TABLE.lock();
        if let Some(index) = table.iter().position(|(entry, _)| *entry == fd) {
            let (_, file) = table.remove(index);
            // SAFETY: `file` was stored in FD_TABLE with an owned reference; fput_file releases that reference.
            unsafe { fput_file(file as *mut File) };
        }
    }
}

unsafe extern "C" {
    fn dma_fence_init(
        fence: *mut DmaFence,
        ops: *const DmaFenceOps,
        lock: *mut Spinlock,
        context: u64,
        seqno: u64,
    );
    fn dma_fence_context_alloc(num: u32) -> u64;
    fn dma_fence_get(fence: *mut DmaFence) -> *mut DmaFence;
    fn dma_fence_put(fence: *mut DmaFence);
    fn dma_fence_signal(fence: *mut DmaFence) -> i32;
    fn dma_fence_set_error(fence: *mut DmaFence, error: i32);
    fn dma_fence_free(fence: *mut DmaFence);
    fn dma_fence_is_signaled(fence: *mut DmaFence) -> bool;
    fn dma_fence_wait(fence: *mut DmaFence, interruptible: bool) -> i32;
    fn dma_fence_chain_find_seqno(fence: *mut *mut DmaFence, point: u64) -> i32;
    fn dma_fence_chain_walk(fence: *mut DmaFence) -> *mut DmaFence;
    fn dma_fence_array_create(
        num_fences: u32,
        fences: *mut *mut DmaFence,
        context: u64,
        seqno: u32,
    ) -> *mut DmaFence;
    fn dma_fence_set_deadline(fence: *mut DmaFence, deadline: i64);
    fn dma_fence_add_callback(
        fence: *mut DmaFence,
        callback: *mut drm_kapi::types::DmaFenceCb,
        function: unsafe extern "C" fn(*mut DmaFence, *mut drm_kapi::types::DmaFenceCb),
    ) -> i32;
    static dma_fence_chain_ops: DmaFenceOps;
    fn anon_inode_getfile(
        name: *const c_char,
        fops: *const c_void,
        private_data: *mut c_void,
        flags: c_int,
    ) -> *mut c_void;
    fn fget(fd: c_int) -> *mut c_void;
    fn get_unused_fd_flags(flags: c_int) -> c_int;
    fn put_unused_fd(fd: c_int);
    fn fd_install(fd: c_int, file: *mut c_void);
    fn fput(file: *mut c_void);
}

/// An owned reference to the Linux `dma_fence` ABI.
struct FenceRef(*mut DmaFence);

// SAFETY: FenceRef owns a strong reference count on a `DmaFence`. In the Linux/drm-kapi model,
// `DmaFence` is thread-safe and protected by an internal spinlock and atomic reference counts,
// allowing owned references to be transferred across threads.
unsafe impl Send for FenceRef {}
// SAFETY: Concurrent operations on `DmaFence` (such as signaling, waiting, and reference acquisition)
// are serialized by the fence's internal lock and atomics, making shared references safe across threads.
unsafe impl Sync for FenceRef {}

impl FenceRef {
    fn get(fence: *mut DmaFence) -> Option<Self> {
        if fence.is_null() {
            None
        } else {
            // SAFETY: `fence` is verified non-null and points to a live `DmaFence`;
            // dma_fence_get increments its reference count for the new FenceRef owner.
            Some(Self(unsafe { dma_fence_get(fence) }))
        }
    }

    fn into_raw(self) -> *mut DmaFence {
        let fence = self.0;
        core::mem::forget(self);
        fence
    }
}

impl Clone for FenceRef {
    fn clone(&self) -> Self {
        // SAFETY: `self.0` is an owned, valid `DmaFence` pointer; dma_fence_get increments its reference count.
        Self(unsafe { dma_fence_get(self.0) })
    }
}

impl Drop for FenceRef {
    fn drop(&mut self) {
        // SAFETY: `self.0` is an owned, valid `DmaFence` pointer; dma_fence_put decrements its reference count and frees it if zero.
        unsafe { dma_fence_put(self.0) };
    }
}

struct SyncobjState {
    fence: Option<FenceRef>,
    pending_eventfds: drm_kapi::types::ListHead,
}

#[repr(C)]
struct SyncobjEventfdWaiter {
    pending_node: drm_kapi::types::ListHead,
    callback: drm_kapi::types::DmaFenceCb,
    fence: *mut DmaFence,
    eventfd_ctx: *mut c_void,
    point: u64,
    flags: u32,
}

enum EventfdReady {
    Fence(FenceRef),
    AlreadySignaled,
}

struct SyncobjWaitQueueRegistration {
    head: *mut WaitQueueHead,
    entry: UnsafeCell<WaitQueueEntry>,
    notifier: SyncobjWaitNotifier,
}

struct SyncobjWaitQueueRegistrations(Vec<SyncobjWaitQueueRegistration>);

struct SyncobjWaitNotifier {
    woken: AtomicBool,
    task: usize,
}

impl SyncobjWaitQueueRegistrations {
    fn new(objects: &[Arc<DrmSyncobj>], task: usize) -> Result<Self, i32> {
        let mut registrations = Vec::new();
        registrations
            .try_reserve_exact(objects.len())
            .map_err(|_| -ENOMEM)?;
        for object in objects {
            let entry = WaitQueueEntry {
                flags: 0,
                private: ptr::null_mut(),
                func: Some(syncobj_submit_waiter_callback),
                entry: ListHead {
                    next: ptr::null_mut(),
                    prev: ptr::null_mut(),
                },
            };
            let head = object.wait.get();
            registrations.push(SyncobjWaitQueueRegistration {
                head,
                entry: UnsafeCell::new(entry),
                notifier: SyncobjWaitNotifier {
                    woken: AtomicBool::new(false),
                    task,
                },
            });
        }
        // Finish allocation and initialization before exposing any callback.
        // Interior-mutability storage keeps shared notifier reads from freezing
        // the linked entry while another task wakes this queue.
        for registration in &registrations {
            let entry = registration.entry.get();
            // SAFETY: Stable Vec storage, retained objects and notifier; removal
            // in Drop synchronizes with callbacks before any allocation dies.
            unsafe {
                (*entry).private = ptr::addr_of!(registration.notifier).cast_mut().cast();
                drm_kapi::list::init_list_head(ptr::addr_of_mut!((*entry).entry));
                add_wait_queue(registration.head, entry);
            }
        }
        Ok(Self(registrations))
    }

    fn woken(&self) -> bool {
        self.0
            .iter()
            .any(|registration| registration.notifier.woken.load(Ordering::Acquire))
    }
}

impl Drop for SyncobjWaitQueueRegistrations {
    fn drop(&mut self) {
        for registration in &self.0 {
            // SAFETY: `registration.head` points to the `WaitQueueHead` where `registration.entry` was added.
            // remove_wait_queue unlinks the entry under the wait-queue spinlock before the entry is dropped.
            unsafe { remove_wait_queue(registration.head, registration.entry.get()) };
        }
    }
}

/// Wait-queue wake callback invoked when a syncobj replaces its fence or adds a timeline point.
///
/// # Safety
/// `entry` must be null or point to a live `WaitQueueEntry` whose `private` field
/// points to an initialized `SyncobjWaitNotifier` that outlives this callback.
unsafe extern "C" fn syncobj_submit_waiter_callback(
    entry: *mut WaitQueueEntry,
    _mode: u32,
    _sync: i32,
    _key: *mut c_void,
) -> i32 {
    if entry.is_null() {
        return 0;
    }
    // SAFETY: `entry` is checked non-null and points to the `WaitQueueEntry` passed to `add_wait_queue`,
    // whose `private` field was set to `registration.notifier`.
    let notifier = unsafe { (*entry).private.cast::<SyncobjWaitNotifier>() };
    if notifier.is_null() {
        return 0;
    }
    // SAFETY: `notifier` is confirmed non-null and points to the live `SyncobjWaitNotifier` in the registration.
    unsafe {
        (*notifier).woken.store(true, Ordering::Release);
        drm_kapi::wait::wake_up_task((*notifier).task);
    }
    1
}

#[repr(C)]
struct SyncobjFenceWaiter {
    callback: DmaFenceCb,
    woken: AtomicBool,
    task: usize,
}

// SAFETY: the callback list is protected by the owning fence lock; the wake
// flag is atomic and the task identity is immutable after registration.
unsafe impl Send for SyncobjFenceWaiter {}
// SAFETY: concurrent access is limited to the fence-locked callback record,
// the atomic wake flag, and the immutable task identity.
unsafe impl Sync for SyncobjFenceWaiter {}

struct FenceCallbackRegistration {
    fence: FenceRef,
    waiter: Arc<SyncobjFenceWaiter>,
    callback_owner: *const SyncobjFenceWaiter,
}

struct FenceCallbackRegistrations(Vec<FenceCallbackRegistration>);

impl FenceCallbackRegistrations {
    fn new(fences: &[(usize, FenceRef)], task: usize) -> Result<Self, i32> {
        let mut registrations = Self(Vec::new());
        registrations
            .0
            .try_reserve_exact(fences.len())
            .map_err(|_| -ENOMEM)?;
        for (_, fence) in fences {
            // SAFETY: `fence.0` is an owned, valid `DmaFence` reference held by `fence`.
            if unsafe { drm_kapi::dma_fence::dma_fence_is_signaled(fence.0) } {
                continue;
            }
            let waiter = Arc::new(SyncobjFenceWaiter {
                callback: DmaFenceCb {
                    node: ListHead {
                        next: ptr::null_mut(),
                        prev: ptr::null_mut(),
                    },
                    func: None,
                },
                woken: AtomicBool::new(false),
                task,
            });
            let callback_owner = Arc::into_raw(Arc::clone(&waiter));
            // SAFETY: the callback is the first repr(C) field. The callback's
            // retained Arc owns the record until signal or successful removal.
            let callback = unsafe { ptr::addr_of_mut!((*callback_owner.cast_mut()).callback) };
            // SAFETY: `fence.0` is a valid DmaFence; `callback` is a valid pointer to waiter.callback;
            // `syncobj_fence_waiter_callback` is an extern C callback expecting a SyncobjFenceWaiter.
            let result = unsafe {
                drm_kapi::dma_fence::dma_fence_add_callback(
                    fence.0,
                    callback,
                    syncobj_fence_waiter_callback,
                )
            };
            if result == 0 {
                registrations.0.push(FenceCallbackRegistration {
                    fence: fence.clone(),
                    waiter,
                    callback_owner,
                });
            } else {
                // SAFETY: a failed add did not publish the callback record.
                unsafe { drop(Arc::from_raw(callback_owner)) };
                if result != -ENOENT {
                    return Err(result);
                }
            }
        }
        Ok(registrations)
    }

    fn woken(&self) -> bool {
        self.0
            .iter()
            .any(|registration| registration.waiter.woken.load(Ordering::Acquire))
    }
}

impl Drop for FenceCallbackRegistrations {
    fn drop(&mut self) {
        for registration in &self.0 {
            // SAFETY: the local Arc keeps the callback record live while it is
            // removed. A false result means the callback owns its retained Arc.
            let callback = unsafe {
                ptr::addr_of_mut!((*Arc::as_ptr(&registration.waiter).cast_mut()).callback)
            };
            // SAFETY: `registration.fence.0` is a valid DmaFence; `callback` is the registered callback record.
            if unsafe {
                drm_kapi::dma_fence::dma_fence_remove_callback(registration.fence.0, callback)
            } {
                // SAFETY: successful removal guarantees the callback will not
                // consume this Arc reference.
                unsafe { drop(Arc::from_raw(registration.callback_owner)) };
            }
        }
    }
}

/// Fence signaling callback invoked by the Linux dma-fence subsystem when a fence signals.
///
/// # Safety
/// `callback` must point to the `DmaFenceCb` embedded as the first field of a live `SyncobjFenceWaiter`,
/// for which one retained `Arc` reference was given to `dma_fence_add_callback`.
unsafe extern "C" fn syncobj_fence_waiter_callback(
    _fence: *mut DmaFence,
    callback: *mut DmaFenceCb,
) {
    // SAFETY: callback records are the first repr(C) field and dma_fence_add_callback
    // retains exactly one Arc reference for this callback invocation.
    let waiter = unsafe { Arc::from_raw(callback.cast::<SyncobjFenceWaiter>()) };
    waiter.woken.store(true, Ordering::Release);
    drm_kapi::wait::wake_up_task(waiter.task);
}

struct SyncobjWaitSnapshot {
    ready: Option<u32>,
    missing: bool,
    fences: Vec<(usize, FenceRef)>,
}

/// Internal syncobj. Its pointer is stored in `drm_file.syncobj_xa` as one
/// `Arc` reference; every core helper returned to a driver owns another.
pub struct DrmSyncobj {
    state: spin::Mutex<SyncobjState>,
    wait: UnsafeCell<WaitQueueHead>,
}

// SAFETY: `state` is guarded by spin::Mutex. Operations on `wait` synchronize internally
// via the wait-queue head spinlock, allowing DrmSyncobj to be transferred across threads.
unsafe impl Send for DrmSyncobj {}
// SAFETY: Concurrent access to `state` is serialized by spin::Mutex, and wait queue operations
// synchronize via internal locks, making shared access across threads safe.
unsafe impl Sync for DrmSyncobj {}

impl DrmSyncobj {
    fn new() -> Result<Arc<Self>, i32> {
        let mut object = Arc::new(Self {
            state: spin::Mutex::new(SyncobjState {
                fence: None,
                // SAFETY: Zeroed MaybeUninit satisfies ListHead layout, and is immediately initialized by init_list_head below.
                pending_eventfds: unsafe { MaybeUninit::zeroed().assume_init() },
            }),
            // SAFETY: Zeroed MaybeUninit satisfies WaitQueueHead layout, and is immediately initialized by init_waitqueue_head below.
            wait: UnsafeCell::new(unsafe { MaybeUninit::zeroed().assume_init() }),
        });
        // SAFETY: We have unique access to `object` via Arc::get_mut; pending_eventfds is properly aligned and allocated.
        unsafe {
            drm_kapi::list::init_list_head(ptr::addr_of_mut!(
                Arc::get_mut(&mut object)
                    .unwrap()
                    .state
                    .get_mut()
                    .pending_eventfds
            ));
        }
        // SAFETY: We have unique access to `object` via Arc::get_mut; wait is properly aligned and allocated.
        init_waitqueue_head(unsafe { &mut *Arc::get_mut(&mut object).unwrap().wait.get() });
        Ok(object)
    }

    fn fence(&self) -> Option<FenceRef> {
        self.state.lock().fence.clone()
    }

    fn replace_fence(&self, fence: Option<FenceRef>) {
        let retired = {
            let mut state = self.state.lock();
            core::mem::replace(&mut state.fence, fence)
        };
        // SAFETY: `self.wait` is initialized in `new()` and lives for the entire lifetime of `self`.
        // wake_up_all acquires the wait-queue's internal spinlock.
        unsafe { wake_up_all(self.wait.get()) };
        drop(retired);
        self.notify_pending_eventfds();
    }

    fn point_available(&self, point: u64) -> bool {
        let Some(head) = self.fence() else {
            return false;
        };
        if point == 0 {
            return true;
        }
        let mut raw = head.into_raw();
        // SAFETY: `raw` is an owned `DmaFence` reference from `head.into_raw()`.
        // `dma_fence_chain_find_seqno` walks the chain and updates `raw` with an owned reference to the matched point.
        let result = unsafe { dma_fence_chain_find_seqno(&mut raw, point) };
        if !raw.is_null() {
            // SAFETY: `raw` is an owned reference returned by `dma_fence_chain_find_seqno` and must be released.
            unsafe { dma_fence_put(raw) };
        }
        result == 0
    }

    fn find_point(&self, point: u64) -> Result<Option<FenceRef>, i32> {
        let Some(head) = self.fence() else {
            return Ok(None);
        };
        if point == 0 {
            return Ok(Some(head));
        }
        let mut raw = head.into_raw();
        // SAFETY: `raw` is an owned `DmaFence` pointer from `head.into_raw()`.
        // `dma_fence_chain_find_seqno` searches for `point` and updates `raw` to an owned reference.
        let result = unsafe { dma_fence_chain_find_seqno(&mut raw, point) };
        if result != 0 {
            if !raw.is_null() {
                // SAFETY: `raw` was updated by `dma_fence_chain_find_seqno` and must be released on failure.
                unsafe { dma_fence_put(raw) };
            }
            return Ok(None);
        }
        if raw.is_null() {
            return create_software_fence(true)
                .map(|fence| Some(FenceRef(fence)))
                .ok_or(-ENOMEM);
        }
        Ok(Some(FenceRef(raw)))
    }

    fn register_eventfd(&self, waiter: *mut SyncobjEventfdWaiter) {
        // SAFETY: `waiter` is a valid, live `SyncobjEventfdWaiter` allocated in drm_syncobj_eventfd_ioctl.
        let point = unsafe { (*waiter).point };
        let ready = {
            let mut state = self.state.lock();
            match eventfd_point(state.fence.clone(), point) {
                Some(ready) => Some(ready),
                None => {
                    // SAFETY: `state.lock()` ensures exclusive access to `state.pending_eventfds`.
                    // `waiter.pending_node` is a valid unlinked ListHead.
                    unsafe {
                        drm_kapi::list::list_add_tail(
                            ptr::addr_of_mut!((*waiter).pending_node),
                            ptr::addr_of_mut!(state.pending_eventfds),
                        );
                    }
                    None
                }
            }
        };
        if let Some(ready) = ready {
            schedule_eventfd(waiter, ready);
        }
    }

    fn take_ready_eventfd(&self) -> Option<(*mut SyncobjEventfdWaiter, EventfdReady)> {
        let mut state = self.state.lock();
        let head = ptr::addr_of_mut!(state.pending_eventfds);
        // SAFETY: `state.lock()` is held; `head` is the initialized `pending_eventfds` list head.
        let mut node = unsafe { (*head).next };
        while node != head {
            let waiter = node.cast::<SyncobjEventfdWaiter>();
            // SAFETY: `node` is an entry linked in `pending_eventfds` protected by `state.lock()`.
            let next = unsafe { (*node).next };
            // SAFETY: `waiter` points to the `SyncobjEventfdWaiter` whose first field is `pending_node`.
            if let Some(ready) = eventfd_point(state.fence.clone(), unsafe { (*waiter).point }) {
                // SAFETY: `node` is an entry in `pending_eventfds` under `state.lock()`.
                unsafe { drm_kapi::list::list_del_init(node) };
                return Some((waiter, ready));
            }
            node = next;
        }
        None
    }

    fn notify_pending_eventfds(&self) {
        while let Some((waiter, ready)) = self.take_ready_eventfd() {
            schedule_eventfd(waiter, ready);
        }
    }

    fn add_point(&self, chain: *mut DmaFenceChain, fence: *mut DmaFence, point: u64) -> i32 {
        if chain.is_null() || fence.is_null() {
            return -EINVAL;
        }
        let retired = {
            let mut state = self.state.lock();
            let previous = state.fence.as_ref().map(|old| old.0);
            // SAFETY: `chain` is non-null and allocated for `DmaFenceChain`; `fence` is checked non-null and valid;
            // `previous` is either null or a valid fence pointer from `state.fence`.
            if unsafe { init_chain(chain, previous, fence, point) } != 0 {
                return -EINVAL;
            }
            // SAFETY: `chain` was initialized as a valid `DmaFenceChain` by `init_chain`.
            let base = unsafe { ptr::addr_of_mut!((*chain).base) };
            state.fence.replace(FenceRef(base))
        };
        // SAFETY: `self.wait` is initialized and valid for the lifetime of `self`; wake_up_all acquires the internal spinlock.
        unsafe { wake_up_all(self.wait.get()) };
        drop(retired);
        self.notify_pending_eventfds();
        0
    }
}

impl Drop for DrmSyncobj {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        let head = ptr::addr_of_mut!(state.pending_eventfds);
        loop {
            // SAFETY: In `drop()`, we have exclusive mutable access to `self`; `head` is the initialized `pending_eventfds` list head.
            let node = unsafe { (*head).next };
            if node == head {
                break;
            }
            // SAFETY: `node` is linked into `pending_eventfds` and unlinked here.
            unsafe { drm_kapi::list::list_del_init(node) };
            destroy_eventfd_waiter(node.cast::<SyncobjEventfdWaiter>());
        }
    }
}

fn eventfd_point(head: Option<FenceRef>, point: u64) -> Option<EventfdReady> {
    let head = head?;
    if point == 0 {
        return Some(EventfdReady::Fence(head));
    }
    let mut raw = head.into_raw();
    // SAFETY: `raw` is an owned `DmaFence` reference from `head.into_raw()`.
    // `dma_fence_chain_find_seqno` walks the chain and assigns `raw` to the matching point fence.
    let result = unsafe { dma_fence_chain_find_seqno(&mut raw, point) };
    if result != 0 {
        if !raw.is_null() {
            // SAFETY: `raw` was returned by `dma_fence_chain_find_seqno` and must be released on failure.
            unsafe { dma_fence_put(raw) };
        }
        None
    } else if raw.is_null() {
        Some(EventfdReady::AlreadySignaled)
    } else {
        Some(EventfdReady::Fence(FenceRef(raw)))
    }
}

fn destroy_eventfd_waiter(waiter: *mut SyncobjEventfdWaiter) {
    if waiter.is_null() {
        return;
    }
    // SAFETY: `waiter` is verified non-null and was allocated via kmalloc in drm_syncobj_eventfd_ioctl.
    // Its fields eventfd_ctx and fence hold owned references to release before freeing the waiter struct.
    unsafe {
        eventfd_ctx_put((*waiter).eventfd_ctx);
        if !(*waiter).fence.is_null() {
            dma_fence_put((*waiter).fence);
        }
        kfree(waiter.cast::<c_void>());
    }
}

fn signal_eventfd_waiter(waiter: *mut SyncobjEventfdWaiter) {
    if waiter.is_null() {
        return;
    }
    // SAFETY: `waiter` is verified non-null; `eventfd_ctx` is a valid eventfd context obtained via eventfd_ctx_fdget.
    unsafe {
        let _ = eventfd_signal((*waiter).eventfd_ctx, 1);
    }
    destroy_eventfd_waiter(waiter);
}

/// Eventfd fence completion callback invoked by the Linux dma-fence signaling subsystem.
///
/// # Safety
/// `callback` must point to the `DmaFenceCb` embedded in a live `SyncobjEventfdWaiter`,
/// positioned immediately following its `pending_node: ListHead`.
unsafe extern "C" fn eventfd_fence_callback(
    _fence: *mut DmaFence,
    callback: *mut drm_kapi::types::DmaFenceCb,
) {
    // SAFETY: `callback` is the address of `waiter.callback`, which immediately follows `waiter.pending_node` (ListHead)
    // in `SyncobjEventfdWaiter`'s repr(C) layout.
    let waiter = unsafe {
        callback
            .cast::<u8>()
            .sub(size_of::<drm_kapi::types::ListHead>())
            .cast::<SyncobjEventfdWaiter>()
    };
    signal_eventfd_waiter(waiter);
}

fn schedule_eventfd(waiter: *mut SyncobjEventfdWaiter, ready: EventfdReady) {
    // SAFETY: `waiter` is non-null and points to a valid `SyncobjEventfdWaiter`.
    if unsafe { (*waiter).flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE } != 0
        || matches!(&ready, EventfdReady::AlreadySignaled)
    {
        signal_eventfd_waiter(waiter);
        return;
    }
    let EventfdReady::Fence(fence) = ready else {
        unreachable!();
    };
    let fence = fence.into_raw();
    // SAFETY: `waiter` is valid; assigning the transferred raw fence ownership.
    unsafe { (*waiter).fence = fence };
    // SAFETY: `fence` is an owned `DmaFence`; `waiter.callback` is valid and aligned; `eventfd_fence_callback` is a valid C callback.
    let ret = unsafe {
        dma_fence_add_callback(
            fence,
            ptr::addr_of_mut!((*waiter).callback),
            eventfd_fence_callback,
        )
    };
    // Linux's -ENOENT means the fence raced to signaled before callback
    // registration; the eventfd must still be signaled exactly once.
    if ret != 0 {
        signal_eventfd_waiter(waiter);
    }
}

/// Return a pointer to the syncobj XArray inside `file`.
///
/// # Safety
/// `file` must point to a valid, live `DrmFile` whose `syncobj_xa` field is properly aligned.
#[inline]
unsafe fn syncobj_xa(file: *mut DrmFile) -> *mut drm_kapi::types::XArray {
    // SAFETY: `file` is guaranteed valid by the caller's precondition.
    unsafe { ptr::addr_of_mut!((*file).syncobj_xa) }
}

fn create_software_fence(signaled: bool) -> Option<*mut DmaFence> {
    let fence = kmalloc(size_of::<DmaFence>(), GFP_KERNEL).cast::<DmaFence>();
    if fence.is_null() {
        return None;
    }
    // SAFETY: `fence` was allocated via kmalloc with `size_of::<DmaFence>()` and checked non-null.
    // `write_bytes` zero-initializes the fence structure; `dma_fence_init` initializes its ops,
    // inline lock, context, and sequence number; `dma_fence_signal` transitions it to signaled if requested.
    unsafe {
        ptr::write_bytes(fence.cast::<u8>(), 0, size_of::<DmaFence>());
        dma_fence_init(
            fence,
            &SOFTWARE_FENCE_OPS,
            ptr::null_mut(),
            dma_fence_context_alloc(1),
            NEXT_FENCE_SEQNO.fetch_add(1, Ordering::Relaxed) as u64,
        );
        if signaled {
            let _ = dma_fence_signal(fence);
        }
    }
    Some(fence)
}

static NEXT_FENCE_SEQNO: AtomicU32 = AtomicU32::new(1);

/// # Safety
/// Callable from C ABI for DmaFenceOps; `_` must be null or point to a valid `DmaFence`.
unsafe extern "C" fn software_fence_driver_name(_: *mut DmaFence) -> *const core::ffi::c_char {
    c"drm-syncobj".as_ptr()
}

/// # Safety
/// Callable from C ABI for DmaFenceOps; `_` must be null or point to a valid `DmaFence`.
unsafe extern "C" fn software_fence_timeline_name(_: *mut DmaFence) -> *const core::ffi::c_char {
    c"software".as_ptr()
}

/// # Safety
/// Callable from C ABI for DmaFenceOps; `_` must be null or point to a valid `DmaFence`.
unsafe extern "C" fn software_fence_enable_signaling(_: *mut DmaFence) -> bool {
    true
}

/// # Safety
/// `fence` must be a valid, live pointer to a `DmaFence` whose reference count reached zero.
unsafe extern "C" fn software_fence_release(fence: *mut DmaFence) {
    // Fence readers may still be inside dma_fence_get_rcu() after the final
    // reference is dropped. Use the Linux dma-fence RCU free path.
    // SAFETY: `fence` is a valid `DmaFence` being released through the `DmaFenceOps` callback.
    unsafe { dma_fence_free(fence) };
}

static SOFTWARE_FENCE_OPS: DmaFenceOps = DmaFenceOps {
    get_driver_name: Some(software_fence_driver_name),
    get_timeline_name: Some(software_fence_timeline_name),
    enable_signaling: Some(software_fence_enable_signaling),
    signaled: None,
    wait: Some(dma_fence_default_wait),
    release: Some(software_fence_release),
    set_deadline: None,
};

unsafe extern "C" {
    fn dma_fence_default_wait(fence: *mut DmaFence, interruptible: bool, timeout: c_long)
    -> c_long;
}

/// Allocate a single timeline chain node over the previous head and new
/// completion fence. The caller serializes `syncobj` replacement.
///
/// # Safety
/// - `chain` must be a valid, aligned, non-null pointer to uninitialized or zeroed `DmaFenceChain` storage of at least `size_of::<DmaFenceChain>()` bytes.
/// - `fence` must be a valid, non-null pointer to an initialized `DmaFence`.
/// - `previous` must be null or point to a valid, initialized `DmaFence`.
unsafe fn init_chain(
    chain: *mut DmaFenceChain,
    previous: Option<*mut DmaFence>,
    fence: *mut DmaFence,
    point: u64,
) -> i32 {
    if chain.is_null() || fence.is_null() {
        return -EINVAL;
    }
    // SAFETY: `chain` is non-null and satisfies DmaFenceChain size and alignment from caller's precondition.
    unsafe { ptr::write_bytes(chain.cast::<u8>(), 0, size_of::<DmaFenceChain>()) };
    let previous = previous.unwrap_or_default();
    let mut previous_point = 0;
    let (context, seqno) = if previous.is_null() {
        // SAFETY: `dma_fence_context_alloc` allocates a unique 64-bit fence context ID.
        (unsafe { dma_fence_context_alloc(1) }, point)
    // SAFETY: `previous` is verified non-null; inspecting ops to check if it is a chain node.
    } else if unsafe { (*previous).ops == ptr::addr_of!(dma_fence_chain_ops).cast() } {
        // SAFETY: `previous` is non-null and has ops matching `dma_fence_chain_ops`.
        previous_point = unsafe { (*previous).seqno };
        if point > previous_point {
            // SAFETY: `previous` is non-null and valid.
            (unsafe { (*previous).context }, point)
        } else {
            (
                // SAFETY: allocating a new timeline context for out-of-order points.
                unsafe { dma_fence_context_alloc(1) },
                point.max(previous_point),
            )
        }
    } else {
        // SAFETY: allocating a new timeline context for non-chain previous fence.
        (unsafe { dma_fence_context_alloc(1) }, point)
    };
    // SAFETY: `chain` is valid, `context`/`seqno` are valid; `dma_fence_init` initializes the base fence;
    // `dma_fence_get` increments reference counts for `previous` (if non-null) and `fence`.
    unsafe {
        dma_fence_init(
            ptr::addr_of_mut!((*chain).base),
            &dma_fence_chain_ops,
            ptr::null_mut(),
            context,
            seqno,
        );
        (*chain).prev = if previous.is_null() {
            ptr::null_mut()
        } else {
            dma_fence_get(previous)
        };
        (*chain).prev_seqno = previous_point;
        (*chain).fence = dma_fence_get(fence);
    }
    0
}

/// Allocate a timeline node on behalf of driver submit paths.
pub fn drm_syncobj_chain_alloc() -> *mut DmaFenceChain {
    kmalloc(size_of::<DmaFenceChain>(), GFP_KERNEL).cast()
}

/// Drop an uninitialized timeline chain allocation.
///
/// # Safety
/// `chain` must be null or point to an uninitialized/unlinked `DmaFenceChain` previously allocated via `drm_syncobj_chain_alloc()`.
pub unsafe fn drm_syncobj_chain_free(chain: *mut DmaFenceChain) {
    if !chain.is_null() {
        // SAFETY: `chain` is confirmed non-null and was allocated via kmalloc in drm_syncobj_chain_alloc.
        unsafe { kfree(chain.cast::<c_void>()) };
    }
}

/// Create an owned Linux dma-fence for asynchronous driver work.
pub fn drm_syncobj_fence_create() -> *mut DmaFence {
    create_software_fence(false).unwrap_or_default()
}

/// Publish a terminal result to a driver-created software fence.
///
/// # Safety
/// `fence` must be null or point to a live, initialized `DmaFence`.
pub unsafe fn drm_syncobj_fence_signal(fence: *mut DmaFence, error: i32) {
    if fence.is_null() {
        return;
    }
    // SAFETY: `fence` is non-null and points to a live `DmaFence`.
    // If error is non-zero, dma_fence_set_error records the status before dma_fence_signal completes it.
    unsafe {
        if error != 0 {
            dma_fence_set_error(fence, error);
        }
        let _ = dma_fence_signal(fence);
    }
}

/// Release one Linux dma-fence reference.
///
/// # Safety
/// `fence` must be null or point to a live `DmaFence` holding an acquired reference.
pub unsafe fn drm_syncobj_fence_put(fence: *mut DmaFence) {
    // SAFETY: `fence` is either null or a valid DmaFence reference to be decremented.
    unsafe { dma_fence_put(fence) };
}

/// Wait on an owned dma-fence dependency from a render submit.
///
/// # Safety
/// `fence` must be null or point to a live, initialized `DmaFence`.
pub unsafe fn drm_syncobj_wait_fence(fence: *mut DmaFence, interruptible: bool) -> i32 {
    if fence.is_null() {
        return -EINVAL;
    }
    // SAFETY: `fence` is verified non-null and points to a valid DmaFence.
    unsafe { dma_fence_wait(fence, interruptible) }
}

/// Resolve an input sync-file descriptor to an owned dma-fence reference.
///
/// # Safety
/// `fd` is a file descriptor passed by value.
pub unsafe fn drm_syncobj_sync_file_get_fence(fd: c_int) -> *mut DmaFence {
    // SAFETY: sync_file_get_fence looks up `fd` in the process file table safely and returns a fence reference or null.
    unsafe { sync_file_get_fence(fd) }
}

/// Create a sync-file wrapper retaining the supplied dma-fence.
///
/// # Safety
/// `fence` must be null or point to a live, initialized `DmaFence`.
pub unsafe fn drm_syncobj_sync_file_create(fence: *mut DmaFence) -> *mut c_void {
    // SAFETY: Caller guarantees `fence` is null or a valid DmaFence reference; sync_file_create wraps it in a sync_file.
    unsafe { sync_file_create(fence) }.cast()
}

/// Return the Linux file pointer at the head of an upstream sync-file wrapper.
///
/// # Safety
/// `sync_file` must be null or point to a live sync_file structure whose first pointer field is `*mut File`.
pub unsafe fn drm_syncobj_sync_file_file(sync_file: *mut c_void) -> *mut c_void {
    if sync_file.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `sync_file` is confirmed non-null; sync_file struct layout begins with the `*mut File` pointer.
    unsafe { *sync_file.cast::<*mut c_void>() }
}

/// Release an unpublished sync-file wrapper.
///
/// # Safety
/// `file` must be null or point to a live `File` object whose reference should be dropped.
pub unsafe fn drm_syncobj_sync_file_fput(file: *mut c_void) {
    // SAFETY: `file` is passed to fput to release one file reference count.
    unsafe { fput(file) };
}

/// Linux `drm_syncobj_open()`.
#[unsafe(no_mangle)]
/// # Safety
/// `file` must be null or point to a live `DrmFile` being initialized.
pub unsafe extern "C" fn drm_syncobj_open(file: *mut DrmFile) {
    if file.is_null() {
        return;
    }
    // SAFETY: `file` is confirmed non-null and points to a valid DrmFile being opened; syncobj_xa is initialized with XA_FLAGS_ALLOC.
    unsafe {
        drm_kapi::xarray::xa_init_flags(&mut (*file).syncobj_xa, drm_kapi::xarray::XA_FLAGS_ALLOC);
    }
}

/// Linux `drm_syncobj_release()`.
#[unsafe(no_mangle)]
/// # Safety
/// `file` must be null or point to a live `DrmFile` being closed. The caller must ensure no concurrent ioctls access `file`.
pub unsafe extern "C" fn drm_syncobj_release(file: *mut DrmFile) {
    if file.is_null() {
        return;
    }
    // SAFETY: `file` is confirmed non-null and valid.
    let xa = unsafe { syncobj_xa(file) };
    // Final DRM-file release cannot race ioctls using its handle table. Take
    // the snapshot outside the XArray spinlock and drop each object after its
    // independently locked erase, since a final fence put may run callbacks.
    // SAFETY: `xa` is a valid pointer to `file.syncobj_xa`; caller guarantees exclusive release access.
    for (handle, _) in drm_kapi::xarray::xa_snapshot(unsafe { &mut *xa }) {
        // SAFETY: `xa` is valid and `&mut *xa` is exclusive during final file release.
        let object = drm_kapi::xarray::xa_erase_irq::<_, DrmSyncobj>(unsafe { &mut *xa }, handle);
        if !object.is_null() {
            // SAFETY: `object` was stored in XArray as an owned `Arc<DrmSyncobj>` raw pointer.
            unsafe { drop(Arc::from_raw(object)) };
        }
    }
    // SAFETY: `xa` is valid and all entries have been erased.
    drm_kapi::xarray::xa_destroy(unsafe { &mut *xa });
}

/// Linux `drm_syncobj_find()`: one owned reference, or NULL for a missing handle.
#[unsafe(no_mangle)]
/// # Safety
/// `file` must be null or point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_find(file: *mut DrmFile, handle: u32) -> *mut DrmSyncobj {
    if file.is_null() || handle == 0 {
        return ptr::null_mut();
    }
    // SAFETY: `file` is confirmed non-null and valid.
    let xa = unsafe { syncobj_xa(file) };
    // SAFETY: `xa` points to the initialized `file.syncobj_xa`.
    drm_kapi::xarray::xa_lock(unsafe { &mut *xa });
    // SAFETY: `xa` lock is held; loading the raw `Arc<DrmSyncobj>` pointer stored at `handle`.
    let syncobj = drm_kapi::xarray::xa_load::<_, DrmSyncobj>(unsafe { &mut *xa }, handle);
    if !syncobj.is_null() {
        // SAFETY: `syncobj` is a valid `Arc<DrmSyncobj>` raw pointer kept live by XArray while the lock is held.
        unsafe { Arc::increment_strong_count(syncobj) };
    }
    // SAFETY: releasing the XArray lock held above.
    drm_kapi::xarray::xa_unlock(unsafe { &mut *xa });
    syncobj
}

/// Linux `drm_syncobj_put()`.
#[unsafe(no_mangle)]
/// # Safety
/// `syncobj` must be null or point to a live `DrmSyncobj` with an acquired strong reference.
pub unsafe extern "C" fn drm_syncobj_put(syncobj: *mut DrmSyncobj) {
    if !syncobj.is_null() {
        // SAFETY: `syncobj` is confirmed non-null and caller guarantees it owns a strong reference count.
        unsafe { Arc::decrement_strong_count(syncobj) };
    }
}

/// Linux syncobj reference acquisition for VFS descriptor wrappers.
#[unsafe(no_mangle)]
/// # Safety
/// `syncobj` must be null or point to a live `DrmSyncobj`.
pub unsafe extern "C" fn drm_syncobj_get(syncobj: *mut DrmSyncobj) -> *mut DrmSyncobj {
    if !syncobj.is_null() {
        // SAFETY: `syncobj` is confirmed non-null and caller guarantees it has an existing live reference.
        unsafe { Arc::increment_strong_count(syncobj) };
    }
    syncobj
}

/// Linux `drm_syncobj_file_fops`: the identity of syncobj descriptors. Its only
/// operation drops the reference the descriptor holds on the syncobj.
pub static DRM_SYNCOBJ_FILE_FOPS: FileOperations = FileOperations {
    release: Some(drm_syncobj_file_release),
    ..FileOperations::empty()
};

/// Linux `drm_syncobj_file_release()`.
///
/// # Safety
/// `file` must be null or point to a live `File` whose `_private_data` holds an owned reference to a `DrmSyncobj`.
unsafe extern "C" fn drm_syncobj_file_release(_inode: *mut Inode, file: *mut File) -> c_int {
    // SAFETY: the file's private data is the syncobj reference `drm_syncobj_get_fd`
    // took for this descriptor; the final fput releases exactly that reference.
    let syncobj = unsafe { (*file)._private_data }.cast::<DrmSyncobj>();
    // SAFETY: `syncobj` was deposited by `drm_syncobj_get_fd` with an incremented strong reference.
    unsafe { drm_syncobj_put(syncobj) };
    0
}

/// Linux `drm_syncobj_get_fd()`: a new descriptor that holds its own reference
/// to `syncobj`. Returns the descriptor or a negative errno.
fn drm_syncobj_get_fd(syncobj: *mut DrmSyncobj) -> Result<c_int, c_int> {
    // SAFETY: `get_unused_fd_flags` takes integer flags and reserves an unused file descriptor.
    let fd = unsafe { get_unused_fd_flags(O_CLOEXEC) };
    if fd < 0 {
        return Err(fd);
    }
    // SAFETY: `anon_inode_getfile` creates an anonymous file with `DRM_SYNCOBJ_FILE_FOPS` and `syncobj` private data.
    let file = unsafe {
        anon_inode_getfile(
            c"syncobj_file".as_ptr(),
            ptr::addr_of!(DRM_SYNCOBJ_FILE_FOPS).cast(),
            syncobj.cast(),
            0,
        )
    };
    if drm_kapi::config::is_err(file) {
        let error = drm_kapi::config::ptr_err(file);
        // SAFETY: `fd` was reserved above and must be returned to the allocator on error.
        unsafe { put_unused_fd(fd) };
        return Err(error);
    }
    // SAFETY: `syncobj` is valid; increment reference count to be owned by the new file descriptor.
    unsafe { drm_syncobj_get(syncobj) };
    // SAFETY: `fd` and `file` are valid uninstalled descriptor and file pointers.
    unsafe { fd_install(fd, file) };
    Ok(fd)
}

fn create_syncobj_handle(file: *mut DrmFile, syncobj: Arc<DrmSyncobj>) -> Result<u32, i32> {
    let raw = Arc::into_raw(syncobj).cast_mut();
    // SAFETY: `file` is a valid DrmFile.
    let xa = unsafe { syncobj_xa(file) };
    let mut handle = 0u32;
    let mut next = 1u32;
    // SAFETY: `xa` points to `file.syncobj_xa`; `raw` is an owned Arc pointer deposited into XArray.
    let ret = drm_kapi::xarray::xa_alloc_cyclic_irq(
        unsafe { &mut *xa },
        &mut handle,
        raw,
        drm_kapi::xarray::XaLimit {
            min: 1,
            max: i32::MAX as u32,
        },
        &mut next,
        GFP_KERNEL,
    );
    if ret != 0 {
        // SAFETY: allocation failed so XArray did not take ownership of `raw`; reconstruct Arc to drop it.
        unsafe { drop(Arc::from_raw(raw)) };
        Err(ret)
    } else {
        Ok(handle)
    }
}

fn remove_syncobj_handle(file: *mut DrmFile, handle: u32) -> Result<(), i32> {
    if handle == 0 {
        return Err(-ENOENT);
    }
    // SAFETY: `file` is a valid DrmFile.
    let xa = unsafe { syncobj_xa(file) };
    // SAFETY: `xa` points to `file.syncobj_xa`; `xa_erase_irq` removes the entry and returns the owned Arc raw pointer.
    let raw = drm_kapi::xarray::xa_erase_irq::<_, DrmSyncobj>(unsafe { &mut *xa }, handle);
    if raw.is_null() {
        Err(-ENOENT)
    } else {
        // SAFETY: `raw` was removed from XArray where it was held as an owned Arc pointer.
        unsafe { drop(Arc::from_raw(raw)) };
        Ok(())
    }
}

/// Linux `drm_syncobj_create()`: a syncobj not yet published to any file.
/// `DRM_SYNCOBJ_CREATE_SIGNALED` assigns the signaled stub and a non-null
/// `fence` replaces it. The caller owns the returned reference.
#[unsafe(no_mangle)]
/// # Safety
/// - `out` must point to a valid, writable `*mut DrmSyncobj` pointer location.
/// - `fence` must be null or point to a live `DmaFence`.
pub unsafe extern "C" fn drm_syncobj_create(
    out: *mut *mut DrmSyncobj,
    flags: u32,
    fence: *mut DmaFence,
) -> c_int {
    if out.is_null() {
        return -EINVAL;
    }
    let object = match DrmSyncobj::new() {
        Ok(object) => object,
        Err(error) => return error,
    };
    if flags & DRM_SYNCOBJ_CREATE_SIGNALED != 0 {
        match signaled_stub() {
            Ok(stub) => object.replace_fence(Some(stub)),
            Err(error) => return error,
        }
    }
    if !fence.is_null() {
        object.replace_fence(FenceRef::get(fence));
    }
    // SAFETY: `out` is checked non-null and caller guarantees it is writable; storing the owned raw Arc pointer.
    unsafe { *out = Arc::into_raw(object).cast_mut() };
    0
}

/// Linux `drm_syncobj_fence_get()`.
#[unsafe(no_mangle)]
/// # Safety
/// `syncobj` must be null or point to a live `DrmSyncobj`.
pub unsafe extern "C" fn drm_syncobj_fence_get(syncobj: *mut DrmSyncobj) -> *mut DmaFence {
    if syncobj.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `syncobj` is confirmed non-null and caller guarantees it points to a valid DrmSyncobj.
    unsafe { &*syncobj }
        .fence()
        .map_or(ptr::null_mut(), FenceRef::into_raw)
}

/// Linux `drm_syncobj_replace_fence()`.
#[unsafe(no_mangle)]
/// # Safety
/// `syncobj` must be null or point to a live `DrmSyncobj`.
/// `fence` must be null or point to a live `DmaFence`.
pub unsafe extern "C" fn drm_syncobj_replace_fence(syncobj: *mut DrmSyncobj, fence: *mut DmaFence) {
    if syncobj.is_null() {
        return;
    }
    let fence = FenceRef::get(fence);
    // SAFETY: `syncobj` is confirmed non-null and caller guarantees it points to a valid DrmSyncobj.
    unsafe { &*syncobj }.replace_fence(fence);
}

/// Linux `drm_syncobj_add_point()` with caller-owned chain storage.
#[unsafe(no_mangle)]
/// # Safety
/// - `syncobj` must be null or point to a live `DrmSyncobj`.
/// - `chain` must be null or point to uninitialized `DmaFenceChain` storage allocated for this point.
/// - `fence` must be null or point to a live `DmaFence`.
pub unsafe extern "C" fn drm_syncobj_add_point(
    syncobj: *mut DrmSyncobj,
    chain: *mut DmaFenceChain,
    fence: *mut DmaFence,
    point: u64,
) {
    if syncobj.is_null() || chain.is_null() || fence.is_null() {
        return;
    }
    // SAFETY: `syncobj`, `chain`, and `fence` are confirmed non-null and satisfy preconditions.
    let _ = unsafe { &*syncobj }.add_point(chain, fence, point);
}

/// Append a point using an internally allocated chain node.
///
/// # Safety
/// `syncobj` must be null or point to a live `DrmSyncobj`.
/// `fence` must be null or point to a live `DmaFence`.
pub unsafe fn drm_syncobj_add_point_auto(
    syncobj: *mut DrmSyncobj,
    fence: *mut DmaFence,
    point: u64,
) -> i32 {
    if syncobj.is_null() || fence.is_null() {
        return -EINVAL;
    }
    let chain = drm_syncobj_chain_alloc();
    if chain.is_null() {
        return -ENOMEM;
    }
    // SAFETY: `syncobj` and `fence` are checked non-null; `chain` was just allocated; add_point initializes chain and fence.
    let ret = unsafe { &*syncobj }.add_point(chain, fence, point);
    if ret != 0 {
        // SAFETY: `add_point` failed so `chain` was not linked into the timeline and must be freed.
        unsafe { drm_syncobj_chain_free(chain) };
    }
    ret
}

/// Linux `drm_syncobj_find_fence()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `file` must point to a live `DrmFile`.
/// - `out` must point to a valid, writable `*mut DmaFence` pointer location.
pub unsafe extern "C" fn drm_syncobj_find_fence(
    file: *mut DrmFile,
    handle: u32,
    point: u64,
    flags: u64,
    out: *mut *mut DmaFence,
) -> i32 {
    if file.is_null() || out.is_null() {
        return -EINVAL;
    }
    // SAFETY: `out` is checked non-null and writable.
    unsafe { *out = ptr::null_mut() };
    if flags & !(DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT as u64) != 0 {
        return -EINVAL;
    }
    // SAFETY: `file` is non-null and points to a live `DrmFile`.
    let syncobj = unsafe { drm_syncobj_find(file, handle) };
    if syncobj.is_null() {
        return -ENOENT;
    }
    let result = loop {
        // SAFETY: `syncobj` is confirmed non-null and holds an acquired strong reference.
        match unsafe { &*syncobj }.find_point(point) {
            Ok(Some(fence)) => break Ok(fence.into_raw()),
            Err(error) => break Err(error),
            Ok(None) if flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT as u64 == 0 => {
                break Err(-EINVAL);
            }
            Ok(None) => {
                let timeout =
                    drm_kapi::primitives::msecs_to_jiffies(WAIT_FOR_SUBMIT_TIMEOUT_JIFFIES);
                let wait = drm_kapi::wait_event_interruptible_timeout!(
                    // SAFETY: `syncobj` is live and its wait field is an initialized WaitQueueHead.
                    unsafe { (*syncobj).wait.get() },
                    // SAFETY: `syncobj` is live and holds an acquired strong reference.
                    unsafe { &*syncobj }.point_available(point),
                    timeout,
                );
                if wait == 0 {
                    break Err(-ETIME);
                }
                if wait < 0 {
                    break Err(-ERESTARTSYS);
                }
            }
        }
    };
    // SAFETY: releasing the strong reference acquired by `drm_syncobj_find` above.
    unsafe { drm_syncobj_put(syncobj) };
    match result {
        Ok(fence) => {
            // SAFETY: `out` is checked non-null and writable.
            unsafe { *out = fence };
            0
        }
        Err(error) => error,
    }
}

/// Signal a software fence and wake generic syncobj waiters. Drivers use this
/// to expose an asynchronous `dma_fence` for their device-owned completion.
///
/// # Safety
/// `fence` must be null or point to a live `DmaFence`.
pub unsafe fn drm_syncobj_signal_fence(fence: *mut DmaFence, error: i32) {
    // SAFETY: `fence` is checked in drm_syncobj_fence_signal.
    unsafe { drm_syncobj_fence_signal(fence, error) };
    // SAFETY: zeroed MaybeUninit satisfies WaitQueueHead layout, initialized by init_waitqueue_head below.
    let mut wait = unsafe { MaybeUninit::<WaitQueueHead>::zeroed().assume_init() };
    init_waitqueue_head(&mut wait);
    // SAFETY: The detached queue is exclusively owned until all callbacks finish.
    unsafe { wake_up_all(ptr::addr_of_mut!(wait)) };
}

/// Install one completion fence in a binary payload, or append it at a
/// timeline point when `point` is nonzero.
///
/// # Safety
/// `syncobj` must be null or point to a live `DrmSyncobj`.
/// `fence` must be null or point to a live `DmaFence`.
pub unsafe fn drm_syncobj_submit_point(
    syncobj: *mut DrmSyncobj,
    fence: *mut DmaFence,
    point: u64,
) -> i32 {
    if point == 0 {
        // SAFETY: `syncobj` and `fence` are passed to `drm_syncobj_replace_fence` which checks nullability.
        unsafe { drm_syncobj_replace_fence(syncobj, fence) };
        0
    } else {
        // SAFETY: `syncobj` and `fence` are passed to `drm_syncobj_add_point_auto` which checks nullability.
        unsafe { drm_syncobj_add_point_auto(syncobj, fence, point) }
    }
}

/// Create a new signaled Linux fence for DRM's signaled-stub semantics.
fn signaled_stub() -> Result<FenceRef, i32> {
    create_software_fence(true).map(FenceRef).ok_or(-ENOMEM)
}

/// Linux `dma_fence_unwrap_merge()` for the transfer-to-timeline path. Chain
/// nodes may not themselves be contained in a new chain node, so flatten the
/// pending contained fences and join them with the Linux fence-array provider.
fn unwrap_merge_fence(fence: *mut DmaFence) -> Result<FenceRef, i32> {
    if fence.is_null() {
        return Err(-EINVAL);
    }
    // SAFETY: `fence` is confirmed non-null; inspecting ops to check if it is a chain node.
    if unsafe { (*fence).ops != ptr::addr_of!(dma_fence_chain_ops).cast() } {
        return FenceRef::get(fence).ok_or(-EINVAL);
    }

    let mut pending = Vec::<*mut DmaFence>::new();
    // SAFETY: `fence` is a valid DmaFence; dma_fence_get acquires an owned reference.
    let mut current = unsafe { dma_fence_get(fence) };
    while !current.is_null() {
        // SAFETY: `current` is non-null; inspecting ops.
        let chain = unsafe { (*current).ops == ptr::addr_of!(dma_fence_chain_ops).cast() };
        if chain {
            // SAFETY: `current` is verified as a DmaFenceChain node; accessing its child fence pointer.
            let child = unsafe { (*current.cast::<DmaFenceChain>()).fence };
            // SAFETY: `child` is null or a valid DmaFence; checking signaled status.
            if !child.is_null() && !unsafe { dma_fence_is_signaled(child) } {
                if pending.try_reserve(1).is_err() {
                    // SAFETY: clean up `current` and all pending fence references on allocation failure.
                    unsafe { dma_fence_put(current) };
                    for child in pending.drain(..) {
                        // SAFETY: `child` is an owned reference in `pending`.
                        unsafe { dma_fence_put(child) };
                    }
                    return Err(-ENOMEM);
                }
                // SAFETY: `child` is confirmed non-null and valid; acquiring a reference for `pending`.
                pending.push(unsafe { dma_fence_get(child) });
            }
            // SAFETY: `current` is a valid chain node; dma_fence_chain_walk consumes current and returns previous.
            current = unsafe { dma_fence_chain_walk(current) };
        } else {
            // SAFETY: `current` is non-null and not a chain node; checking signaled status.
            if !unsafe { dma_fence_is_signaled(current) } {
                if pending.try_reserve(1).is_err() {
                    // SAFETY: `current` and pending references must be released on failure.
                    unsafe { dma_fence_put(current) };
                    for child in pending.drain(..) {
                        // SAFETY: `child` is an owned reference in `pending`.
                        unsafe { dma_fence_put(child) };
                    }
                    return Err(-ENOMEM);
                }
                pending.push(current);
            } else {
                // SAFETY: `current` is signaled and not needed; release reference.
                unsafe { dma_fence_put(current) };
            }
            current = ptr::null_mut();
        }
    }

    match pending.len() {
        0 => signaled_stub(),
        1 => Ok(FenceRef(pending.pop().unwrap())),
        count if count <= u32::MAX as usize => {
            let Some(bytes) = count.checked_mul(size_of::<*mut DmaFence>()) else {
                for child in pending.drain(..) {
                    // SAFETY: releasing acquired fence references on overflow.
                    unsafe { dma_fence_put(child) };
                }
                return Err(-ENOMEM);
            };
            let fences = kmalloc(bytes, GFP_KERNEL).cast::<*mut DmaFence>();
            if fences.is_null() {
                for child in pending.drain(..) {
                    // SAFETY: releasing acquired fence references on allocation failure.
                    unsafe { dma_fence_put(child) };
                }
                return Err(-ENOMEM);
            }
            // SAFETY: `pending` has `count` valid pointers; `fences` has allocated capacity `count * sizeof(*mut DmaFence)`; disjoint.
            unsafe { ptr::copy_nonoverlapping(pending.as_ptr(), fences, count) };
            // SAFETY: `fences` is allocated with `count` pointers; `dma_fence_array_create` takes ownership of `fences`.
            let array = unsafe {
                dma_fence_array_create(
                    count as u32,
                    fences,
                    dma_fence_context_alloc(1),
                    NEXT_FENCE_SEQNO.fetch_add(1, Ordering::Relaxed),
                )
            };
            if array.is_null() {
                // SAFETY: array creation failed; free the `fences` allocation and release pending fence references.
                unsafe { kfree(fences.cast::<c_void>()) };
                for child in pending.drain(..) {
                    // SAFETY: `child` is an owned reference in `pending`.
                    unsafe { dma_fence_put(child) };
                }
                Err(-ENOMEM)
            } else {
                // The fence array now owns each reference and the pointer
                // vector; only the temporary Vec storage remains to drop.
                pending.clear();
                Ok(FenceRef(array))
            }
        }
        _ => {
            for child in pending.drain(..) {
                // SAFETY: releasing acquired fence references on count error.
                unsafe { dma_fence_put(child) };
            }
            Err(-ENOMEM)
        }
    }
}

fn create_syncobj(signaled: bool) -> Result<Arc<DrmSyncobj>, i32> {
    let object = DrmSyncobj::new()?;
    if signaled {
        object.replace_fence(Some(signaled_stub()?));
    }
    Ok(object)
}

fn copy_handles(address: u64, count: u32) -> Result<Vec<u32>, i32> {
    let count = count as usize;
    if count > MAX_SYNCOBJ_COUNT || (count != 0 && address == 0) {
        return Err(-EINVAL);
    }
    let mut handles = Vec::new();
    handles.try_reserve_exact(count).map_err(|_| -ENOMEM)?;
    handles.resize(count, 0);
    if count != 0
        // SAFETY: `handles` has reserved capacity and length `count`; copy_from_user checks user bounds and copies `count * 4` bytes.
        && unsafe {
            copy_from_user(
                handles.as_mut_ptr().cast(),
                address as usize as *const c_void,
                count * size_of::<u32>(),
            )
        } != 0
    {
        return Err(-EFAULT);
    }
    Ok(handles)
}

fn find_syncobjs(file: *mut DrmFile, handles: &[u32]) -> Result<Vec<Arc<DrmSyncobj>>, i32> {
    let mut objects = Vec::new();
    objects
        .try_reserve_exact(handles.len())
        .map_err(|_| -ENOMEM)?;
    for handle in handles {
        // SAFETY: `file` is a valid DrmFile pointer.
        let raw = unsafe { drm_syncobj_find(file, *handle) };
        if raw.is_null() {
            return Err(-ENOENT);
        }
        // SAFETY: `raw` is a non-null pointer returned by `drm_syncobj_find` with an incremented strong count.
        objects.push(unsafe { Arc::from_raw(raw) });
    }
    Ok(objects)
}

fn read_points(address: u64, count: usize) -> Result<Vec<u64>, i32> {
    if count > MAX_SYNCOBJ_COUNT {
        return Err(-EINVAL);
    }
    let mut points = Vec::new();
    points.try_reserve_exact(count).map_err(|_| -ENOMEM)?;
    points.resize(count, 0);
    if address != 0
        && count != 0
        // SAFETY: `points` has reserved capacity and length `count`; copy_from_user checks user bounds and copies `count * 8` bytes.
        && unsafe {
            copy_from_user(
                points.as_mut_ptr().cast(),
                address as usize as *const c_void,
                count * size_of::<u64>(),
            )
        } != 0
    {
        return Err(-EFAULT);
    }
    Ok(points)
}

fn wait_timeout_jiffies(timeout_nsec: i64) -> u64 {
    let now = drm_kapi::primitives::ktime_get().max(0) as u64;
    if timeout_nsec <= 0 || timeout_nsec as u64 <= now {
        return 0;
    }
    let remaining_ms = (timeout_nsec as u64 - now).saturating_add(999_999) / 1_000_000;
    drm_kapi::primitives::msecs_to_jiffies(remaining_ms).max(1)
}

fn chain_query_point(fence: Option<FenceRef>, last_submitted: bool) -> u64 {
    let Some(head) = fence else {
        return 0;
    };
    // SAFETY: `head.0` is a valid DmaFence from FenceRef; inspecting ops to check if it is a chain node.
    if unsafe { (*head.0).ops != ptr::addr_of!(dma_fence_chain_ops).cast() } {
        return 0;
    }
    if last_submitted {
        // SAFETY: `head.0` is a valid chain node; reading seqno.
        return unsafe { (*head.0).seqno };
    }
    // SAFETY: `head.0` is a valid chain node; reading context.
    let context = unsafe { (*head.0).context };
    let mut last = head.clone();
    // SAFETY: `head.0` is a valid DmaFence; dma_fence_get acquires an owned reference for current.
    let mut current = FenceRef(unsafe { dma_fence_get(head.0) });
    loop {
        // SAFETY: `current.0` is a valid DmaFence; comparing context.
        if unsafe { (*current.0).context != context } {
            break;
        }
        last = current.clone();
        // SAFETY: `current.into_raw()` is an owned chain node consumed by dma_fence_chain_walk.
        let previous = unsafe { dma_fence_chain_walk(current.into_raw()) };
        if previous.is_null() {
            break;
        }
        current = FenceRef(previous);
    }
    // SAFETY: `last.0` is a valid DmaFence; checking signaled status.
    if unsafe { dma_fence_is_signaled(last.0) } {
        // SAFETY: `last.0` is valid; reading seqno.
        unsafe { (*last.0).seqno }
    // SAFETY: `last.0` is valid; checking ops.
    } else if unsafe { (*last.0).ops == ptr::addr_of!(dma_fence_chain_ops).cast() } {
        // SAFETY: `last.0` is a valid DmaFenceChain node; reading prev_seqno.
        unsafe { (*last.0.cast::<DmaFenceChain>()).prev_seqno }
    } else {
        0
    }
}

fn wait_syncobjs(
    objects: &[Arc<DrmSyncobj>],
    points: &[u64],
    flags: u32,
    timeout_nsec: i64,
    deadline_nsec: u64,
) -> Result<u32, i32> {
    if objects.is_empty() || points.len() != objects.len() {
        return Err(-EINVAL);
    }
    let wait_all = flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL != 0;
    let relaxed = flags
        & (DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE)
        != 0;
    let wait_available = flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE != 0;
    let timeout_jiffies = wait_timeout_jiffies(timeout_nsec);
    let expiry = drm_kapi::primitives::ktime_get()
        .saturating_add(drm_kapi::primitives::jiffies_to_nsecs(timeout_jiffies) as i64);
    let mut deadline_set = false;
    loop {
        let mut snapshot =
            syncobj_wait_snapshot(objects, points, wait_all, relaxed, wait_available)?;
        if let Some(first) = snapshot.ready {
            return Ok(first);
        }
        if drm_kapi::signal::signal_pending_current() {
            return Err(-ERESTARTSYS);
        }
        let now = drm_kapi::primitives::ktime_get();
        if timeout_jiffies == 0 || now >= expiry {
            return Err(-ETIME);
        }
        let task = drm_kapi::task_runtime::current_task_id()
            .and_then(|task| usize::try_from(task).ok())
            .filter(|task| *task != 0)
            .ok_or(-EOPNOTSUPP)?;
        // Register submit/replacement notifications before taking the fence
        // snapshot. Its notifier flag closes the missed-wakeup window between
        // a replacement and the interruptible wait's first predicate check.
        let object_waiters = SyncobjWaitQueueRegistrations::new(objects, task)?;
        snapshot = syncobj_wait_snapshot(objects, points, wait_all, relaxed, wait_available)?;
        if let Some(first) = snapshot.ready {
            drop(object_waiters);
            return Ok(first);
        }
        if drm_kapi::signal::signal_pending_current() {
            return Err(-ERESTARTSYS);
        }
        let now = drm_kapi::primitives::ktime_get();
        if now >= expiry {
            return Err(-ETIME);
        }
        if !deadline_set && flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE != 0 && deadline_nsec != 0
        {
            for (_, fence) in &snapshot.fences {
                // SAFETY: `fence.0` is a valid `DmaFence` reference from `snapshot.fences`.
                unsafe { dma_fence_set_deadline(fence.0, deadline_nsec as i64) };
            }
            deadline_set = true;
        }
        let remaining_ns = expiry.saturating_sub(now).max(0) as u64;
        let fence_waiters = FenceCallbackRegistrations::new(&snapshot.fences, task)?;
        // SAFETY: Zeroed MaybeUninit satisfies WaitQueueHead layout, initialized by init_waitqueue_head below.
        let mut queue = unsafe { MaybeUninit::<WaitQueueHead>::zeroed().assume_init() };
        init_waitqueue_head(&mut queue);
        let remaining_jiffies = remaining_ns
            .saturating_mul(u64::from(drm_kapi::config::CONFIG_HZ))
            .div_ceil(1_000_000_000)
            .max(1)
            .min(c_long::MAX as u64) as c_long;
        let wait = drm_kapi::wait_event_interruptible_timeout!(
            queue,
            drm_kapi::signal::signal_pending_current()
                || object_waiters.woken()
                || fence_waiters.woken()
                || matches!(
                    syncobj_wait_ready(objects, points, wait_all, relaxed, wait_available),
                    Ok(true) | Err(_)
                ),
            remaining_jiffies,
        );
        drop(fence_waiters);
        drop(object_waiters);
        if wait < 0 {
            return Err(wait as i32);
        }
    }
}

/// Snapshot the requested points before registering waiters, so the wait loop
/// can safely retain every fence while its completion callback is installed.
fn syncobj_wait_snapshot(
    objects: &[Arc<DrmSyncobj>],
    points: &[u64],
    wait_all: bool,
    relaxed: bool,
    wait_available: bool,
) -> Result<SyncobjWaitSnapshot, i32> {
    let mut snapshot = SyncobjWaitSnapshot {
        ready: None,
        missing: false,
        fences: Vec::new(),
    };
    snapshot
        .fences
        .try_reserve_exact(objects.len())
        .map_err(|_| -ENOMEM)?;
    let mut ready_count = 0usize;
    let mut first = u32::MAX;
    for (index, (object, point)) in objects.iter().zip(points).enumerate() {
        let Some(fence) = object.find_point(*point)? else {
            snapshot.missing = true;
            continue;
        };
        // SAFETY: `fence.0` is an owned, valid `DmaFence` pointer returned by find_point.
        if wait_available || unsafe { dma_fence_is_signaled(fence.0) } {
            ready_count += 1;
            if first == u32::MAX {
                first = index as u32;
            }
        }
        snapshot.fences.push((index, fence));
    }
    if if wait_all {
        !snapshot.missing && ready_count == objects.len()
    } else {
        ready_count != 0
    } {
        snapshot.ready = Some(if wait_all { u32::MAX } else { first });
    } else if snapshot.missing && !relaxed {
        return Err(-EINVAL);
    }
    Ok(snapshot)
}

/// Inspect requested points without sleeping. Each loop also checks this from
/// its interruptible wait predicate after fencing callbacks or submit waiters
/// wake the task.
fn syncobj_wait_ready(
    objects: &[Arc<DrmSyncobj>],
    points: &[u64],
    wait_all: bool,
    relaxed: bool,
    wait_available: bool,
) -> Result<bool, i32> {
    let mut ready_count = 0usize;
    let mut missing = false;
    for (object, point) in objects.iter().zip(points) {
        let Some(fence) = object.find_point(*point)? else {
            missing = true;
            continue;
        };
        // SAFETY: `fence.0` is an owned, valid `DmaFence` pointer returned by find_point.
        if wait_available || unsafe { dma_fence_is_signaled(fence.0) } {
            ready_count += 1;
        }
    }
    if if wait_all {
        !missing && ready_count == objects.len()
    } else {
        ready_count != 0
    } {
        return Ok(true);
    }
    if missing && !relaxed {
        return Err(-EINVAL);
    }
    Ok(false)
}

/// Return whether the given DRM driver feature flag is set for `dev`.
///
/// # Safety
/// `dev` must be null or point to a live `DrmDevice`.
#[inline]
unsafe fn feature(dev: *mut DrmDevice, flag: u32) -> bool {
    // SAFETY: `dev` is confirmed non-null and caller guarantees it points to a live DrmDevice.
    !dev.is_null() && unsafe { (*dev).driver_features & flag != 0 }
}

/// Linux `drm_syncobj_create_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned, readable and writable `DrmSyncobjCreate` structure.
/// - `file` must point to a live `DrmFile` associated with the calling process.
pub unsafe extern "C" fn drm_syncobj_create_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjCreate buffer.
    let args = unsafe { &mut *data.cast::<DrmSyncobjCreate>() };
    if args.flags & !DRM_SYNCOBJ_CREATE_SIGNALED != 0 {
        return -EINVAL;
    }
    match create_syncobj(args.flags & DRM_SYNCOBJ_CREATE_SIGNALED != 0)
        .and_then(|object| create_syncobj_handle(file, object))
    {
        Ok(handle) => {
            args.handle = handle;
            0
        }
        Err(error) => error,
    }
}

/// Linux `drm_syncobj_destroy_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned `DrmSyncobjDestroy` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_destroy_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjDestroy buffer.
    let args = unsafe { &*data.cast::<DrmSyncobjDestroy>() };
    if args.pad != 0 {
        return -EINVAL;
    }
    match remove_syncobj_handle(file, args.handle) {
        Ok(()) => 0,
        Err(error) => error,
    }
}

/// Linux `drm_syncobj_handle_to_fd_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned, readable and writable `DrmSyncobjHandle` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_handle_to_fd_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjHandle buffer.
    let args = unsafe { &mut *data.cast::<DrmSyncobjHandle>() };
    let flags =
        DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE | DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE;
    if args.pad != 0 || args.flags & !flags != 0 {
        return -EINVAL;
    }
    let point = if args.flags & DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE != 0 {
        args.point
    } else {
        0
    };
    if args.flags & DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE == 0 {
        if args.point != 0 {
            return -EINVAL;
        }
        // SAFETY: `file` is a valid DrmFile pointer.
        let syncobj = unsafe { drm_syncobj_find(file, args.handle) };
        if syncobj.is_null() {
            return -EINVAL;
        }
        let ret = drm_syncobj_get_fd(syncobj);
        // SAFETY: releasing the reference acquired by `drm_syncobj_find`.
        unsafe { drm_syncobj_put(syncobj) };
        return match ret {
            Ok(fd) => {
                args.fd = fd;
                0
            }
            Err(error) => error,
        };
    }

    // Linux drm_syncobj_export_sync_file(): reserve the descriptor first.
    // SAFETY: reserving an unused descriptor with O_CLOEXEC.
    let fd = unsafe { get_unused_fd_flags(O_CLOEXEC) };
    if fd < 0 {
        return fd;
    }
    let mut fence = ptr::null_mut();
    // SAFETY: `file` is valid; `fence` is a local pointer.
    let ret = unsafe { drm_syncobj_find_fence(file, args.handle, point, 0, &mut fence) };
    if ret != 0 {
        // SAFETY: releasing unused descriptor on fence lookup failure.
        unsafe { put_unused_fd(fd) };
        return ret;
    }
    // SAFETY: `fence` is a valid DmaFence reference returned by find_fence.
    let sync_file = unsafe { sync_file_create(fence) };
    // SAFETY: `fence` reference is now owned by `sync_file`; release our local reference.
    unsafe { dma_fence_put(fence) };
    if sync_file.is_null() {
        // SAFETY: sync_file creation failed; release the reserved descriptor.
        unsafe { put_unused_fd(fd) };
        return -EINVAL;
    }
    // SAFETY: `sync_file` is the live sync_file `sync_file_create` returned.
    unsafe { fd_install(fd, (*sync_file).file.cast()) };
    args.fd = fd;
    0
}

/// Linux `drm_syncobj_fd_to_handle_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned, readable and writable `DrmSyncobjHandle` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_fd_to_handle_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjHandle buffer.
    let args = unsafe { &mut *data.cast::<DrmSyncobjHandle>() };
    let flags =
        DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE | DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE;
    if args.pad != 0 || args.flags & !flags != 0 {
        return -EINVAL;
    }
    if args.flags & DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE == 0 {
        if args.point != 0 {
            return -EINVAL;
        }
        // Linux drm_syncobj_fd_to_handle(): the descriptor must be a syncobj file.
        // SAFETY: `fget` retrieves the file pointer corresponding to `args.fd` safely.
        let linux_file = unsafe { fget(args.fd) }.cast::<File>();
        if linux_file.is_null() {
            return -EINVAL;
        }
        if !ptr::eq(
            // SAFETY: `linux_file` is confirmed non-null; reading its file_operations pointer.
            unsafe { (*linux_file).f_op },
            ptr::addr_of!(DRM_SYNCOBJ_FILE_FOPS),
        ) {
            // SAFETY: releasing the file reference obtained from fget.
            unsafe { fput_file(linux_file) };
            return -EINVAL;
        }
        // The handle table takes its own reference; the fd reference is dropped below.
        // SAFETY: `linux_file` is confirmed to use `DRM_SYNCOBJ_FILE_FOPS`, so `_private_data` holds a valid `DrmSyncobj` pointer.
        let syncobj = unsafe { (*linux_file)._private_data }.cast::<DrmSyncobj>();
        // SAFETY: acquiring a new strong reference for the imported handle.
        unsafe { drm_syncobj_get(syncobj) };
        // SAFETY: releasing the file reference from fget.
        unsafe { fput_file(linux_file) };
        // SAFETY: the reference taken above is the one `Arc::from_raw` consumes.
        let syncobj = unsafe { Arc::from_raw(syncobj) };
        return match create_syncobj_handle(file, syncobj) {
            Ok(handle) => {
                args.handle = handle;
                0
            }
            Err(error) => error,
        };
    }

    // SAFETY: `sync_file_get_fence` retrieves the fence associated with `args.fd`.
    let fence = unsafe { sync_file_get_fence(args.fd) };
    if fence.is_null() {
        return -EINVAL;
    }
    // SAFETY: `file` is a valid DrmFile pointer.
    let syncobj = unsafe { drm_syncobj_find(file, args.handle) };
    if syncobj.is_null() {
        // SAFETY: releasing fence reference on missing syncobj handle.
        unsafe { dma_fence_put(fence) };
        return -ENOENT;
    }
    let point = if args.flags & DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE != 0 {
        args.point
    } else {
        0
    };
    // SAFETY: `syncobj` and `fence` are confirmed valid, non-null pointers.
    let ret = unsafe { drm_syncobj_submit_point(syncobj, fence, point) };
    // SAFETY: releasing local references to fence and syncobj after submission.
    unsafe {
        dma_fence_put(fence);
        drm_syncobj_put(syncobj);
    }
    ret
}

/// Linux `drm_syncobj_transfer_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned `DrmSyncobjTransfer` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_transfer_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ_TIMELINE) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjTransfer buffer.
    let args = unsafe { &*data.cast::<DrmSyncobjTransfer>() };
    if args.pad != 0 || args.flags & !(DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT) != 0 {
        return -EINVAL;
    }
    // SAFETY: `file` is a valid DrmFile pointer.
    let destination = unsafe { drm_syncobj_find(file, args.dst_handle) };
    if destination.is_null() {
        return -ENOENT;
    }
    let mut fence = ptr::null_mut();
    // SAFETY: `file` is valid; `fence` is a local pointer.
    let ret = unsafe {
        drm_syncobj_find_fence(
            file,
            args.src_handle,
            args.src_point,
            args.flags as u64,
            &mut fence,
        )
    };
    if ret != 0 {
        // SAFETY: releasing destination syncobj reference on fence lookup failure.
        unsafe { drm_syncobj_put(destination) };
        return ret;
    }
    let timeline_fence = if args.dst_point != 0 {
        match unwrap_merge_fence(fence) {
            Ok(fence) => Some(fence),
            Err(error) => {
                // SAFETY: releasing fence and destination references on merge failure.
                unsafe {
                    dma_fence_put(fence);
                    drm_syncobj_put(destination);
                }
                return error;
            }
        }
    } else {
        None
    };
    let submit_fence = timeline_fence.as_ref().map_or(fence, |f| f.0);
    // SAFETY: `destination` and `submit_fence` are verified valid, non-null pointers.
    let ret = unsafe { drm_syncobj_submit_point(destination, submit_fence, args.dst_point) };
    // SAFETY: releasing the original `fence` reference and `destination` reference.
    unsafe {
        dma_fence_put(fence);
        drm_syncobj_put(destination);
    }
    drop(timeline_fence);
    ret
}

/// Linux `drm_syncobj_wait_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned, readable and writable `DrmSyncobjWait` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_wait_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjWait buffer.
    let args = unsafe { &mut *data.cast::<DrmSyncobjWait>() };
    let allowed = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL
        | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT
        | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE;
    if args.flags & !allowed != 0 || args.pad != 0 {
        return -EINVAL;
    }
    if args.count_handles == 0 {
        return 0;
    }
    let handles = match copy_handles(args.handles, args.count_handles) {
        Ok(handles) => handles,
        Err(error) => return error,
    };
    let objects = match find_syncobjs(file, &handles) {
        Ok(objects) => objects,
        Err(error) => return error,
    };
    let points = alloc::vec![0u64; objects.len()];
    match wait_syncobjs(
        &objects,
        &points,
        args.flags,
        args.timeout_nsec,
        args.deadline_nsec,
    ) {
        Ok(first) => {
            args.first_signaled = first;
            0
        }
        Err(error) => error,
    }
}

/// Linux `drm_syncobj_timeline_wait_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned, readable and writable `DrmSyncobjTimelineWait` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_timeline_wait_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ_TIMELINE) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjTimelineWait buffer.
    let args = unsafe { &mut *data.cast::<DrmSyncobjTimelineWait>() };
    let allowed = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL
        | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT
        | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE
        | DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE;
    if args.flags & !allowed != 0 || args.pad != 0 {
        return -EINVAL;
    }
    if args.count_handles == 0 {
        return 0;
    }
    let handles = match copy_handles(args.handles, args.count_handles) {
        Ok(handles) => handles,
        Err(error) => return error,
    };
    let objects = match find_syncobjs(file, &handles) {
        Ok(objects) => objects,
        Err(error) => return error,
    };
    let points = match read_points(args.points, objects.len()) {
        Ok(points) => points,
        Err(error) => return error,
    };
    match wait_syncobjs(
        &objects,
        &points,
        args.flags,
        args.timeout_nsec,
        args.deadline_nsec,
    ) {
        Ok(first) => {
            args.first_signaled = first;
            0
        }
        Err(error) => error,
    }
}

/// Linux `drm_syncobj_eventfd_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned `DrmSyncobjEventfd` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_eventfd_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ_TIMELINE) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjEventfd buffer.
    let args = unsafe { &*data.cast::<DrmSyncobjEventfd>() };
    if args.flags & !DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE != 0 || args.pad != 0 {
        return -EINVAL;
    }
    // SAFETY: `file` is a valid DrmFile pointer.
    let syncobj = unsafe { drm_syncobj_find(file, args.handle) };
    if syncobj.is_null() {
        return -ENOENT;
    }
    // SAFETY: `eventfd_ctx_fdget` safely looks up `args.fd` in the calling process file table.
    let eventfd_ctx = unsafe { eventfd_ctx_fdget(args.fd) };
    if drm_kapi::config::is_err(eventfd_ctx) {
        let error = drm_kapi::config::ptr_err(eventfd_ctx);
        // SAFETY: releasing acquired syncobj reference when eventfd_ctx lookup returns an error pointer.
        unsafe { drm_syncobj_put(syncobj) };
        return error;
    }
    if eventfd_ctx.is_null() {
        // SAFETY: releasing acquired syncobj reference when eventfd_ctx is null.
        unsafe { drm_syncobj_put(syncobj) };
        return -EINVAL;
    }
    let waiter =
        kmalloc(size_of::<SyncobjEventfdWaiter>(), GFP_KERNEL).cast::<SyncobjEventfdWaiter>();
    if waiter.is_null() {
        // SAFETY: `waiter` allocation failed; clean up acquired `eventfd_ctx` and `syncobj` references.
        unsafe {
            eventfd_ctx_put(eventfd_ctx);
            drm_syncobj_put(syncobj);
        }
        return -ENOMEM;
    }
    // SAFETY: `waiter` is non-null and allocated for `SyncobjEventfdWaiter`; `ptr::write_bytes` zero-initializes it;
    // `init_list_head` initializes `pending_node`; fields are populated with acquired references;
    // `register_eventfd` takes ownership of `waiter`; and `drm_syncobj_put` drops the local syncobj reference.
    unsafe {
        ptr::write_bytes(waiter, 0, 1);
        drm_kapi::list::init_list_head(ptr::addr_of_mut!((*waiter).pending_node));
        (*waiter).eventfd_ctx = eventfd_ctx;
        (*waiter).point = args.point;
        (*waiter).flags = args.flags;
        (*syncobj).register_eventfd(waiter);
        drm_syncobj_put(syncobj);
    }
    0
}

/// Linux `drm_syncobj_reset_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned `DrmSyncobjArray` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_reset_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjArray buffer.
    let args = unsafe { &*data.cast::<DrmSyncobjArray>() };
    if args.pad != 0 || args.count_handles == 0 {
        return -EINVAL;
    }
    let handles = match copy_handles(args.handles, args.count_handles) {
        Ok(handles) => handles,
        Err(error) => return error,
    };
    let objects = match find_syncobjs(file, &handles) {
        Ok(objects) => objects,
        Err(error) => return error,
    };
    for object in objects {
        object.replace_fence(None);
    }
    0
}

/// Linux `drm_syncobj_signal_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned `DrmSyncobjArray` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_signal_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjArray buffer.
    let args = unsafe { &*data.cast::<DrmSyncobjArray>() };
    if args.pad != 0 || args.count_handles == 0 {
        return -EINVAL;
    }
    let handles = match copy_handles(args.handles, args.count_handles) {
        Ok(handles) => handles,
        Err(error) => return error,
    };
    let objects = match find_syncobjs(file, &handles) {
        Ok(objects) => objects,
        Err(error) => return error,
    };
    let fence = match signaled_stub() {
        Ok(fence) => fence,
        Err(error) => return error,
    };
    for object in objects {
        object.replace_fence(Some(fence.clone()));
    }
    0
}

/// Linux `drm_syncobj_timeline_signal_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned `DrmSyncobjTimelineArray` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_timeline_signal_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ_TIMELINE) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjTimelineArray buffer.
    let args = unsafe { &*data.cast::<DrmSyncobjTimelineArray>() };
    if args.flags != 0 || args.count_handles == 0 {
        return -EINVAL;
    }
    let handles = match copy_handles(args.handles, args.count_handles) {
        Ok(handles) => handles,
        Err(error) => return error,
    };
    let points = match read_points(args.points, handles.len()) {
        Ok(points) => points,
        Err(error) => return error,
    };
    let objects = match find_syncobjs(file, &handles) {
        Ok(objects) => objects,
        Err(error) => return error,
    };
    // Allocate every fence and chain node before publishing any point. A
    // later allocation failure must not leave a prefix of the requested
    // timelines signaled.
    let mut fences = Vec::new();
    let mut chains = Vec::new();
    if fences.try_reserve_exact(objects.len()).is_err()
        || chains.try_reserve_exact(objects.len()).is_err()
    {
        return -ENOMEM;
    }
    for _ in 0..objects.len() {
        let fence = match signaled_stub() {
            Ok(fence) => fence,
            Err(error) => {
                for chain in chains.drain(..) {
                    // SAFETY: `chain` was allocated via drm_syncobj_chain_alloc and not yet linked; freeing on stub allocation error.
                    unsafe { drm_syncobj_chain_free(chain) };
                }
                return error;
            }
        };
        let chain = drm_syncobj_chain_alloc();
        if chain.is_null() {
            for chain in chains.drain(..) {
                // SAFETY: `chain` was allocated via drm_syncobj_chain_alloc and not yet linked; freeing on chain allocation error.
                unsafe { drm_syncobj_chain_free(chain) };
            }
            return -ENOMEM;
        }
        fences.push(fence);
        chains.push(chain);
    }
    for (index, (object, point)) in objects.iter().zip(points).enumerate() {
        let chain = core::mem::take(&mut chains[index]);
        let ret = object.add_point(chain, fences[index].0, point);
        if ret != 0 {
            // SAFETY: `add_point` failed so `chain` was not linked and must be freed.
            unsafe { drm_syncobj_chain_free(chain) };
            for chain in chains.drain(..).filter(|chain| !chain.is_null()) {
                // SAFETY: freeing remaining unpublished chain nodes after failure.
                unsafe { drm_syncobj_chain_free(chain) };
            }
            return ret;
        }
    }
    0
}

/// Linux `drm_syncobj_query_ioctl()`.
#[unsafe(no_mangle)]
/// # Safety
/// - `dev` must be null or point to a live `DrmDevice`.
/// - `data` must point to a valid, aligned `DrmSyncobjTimelineArray` structure.
/// - `file` must point to a live `DrmFile`.
pub unsafe extern "C" fn drm_syncobj_query_ioctl(
    dev: *mut DrmDevice,
    data: *mut c_void,
    file: *mut DrmFile,
) -> i32 {
    // SAFETY: `dev` is null or points to a live DrmDevice.
    if !unsafe { feature(dev, DRIVER_SYNCOBJ_TIMELINE) } {
        return -EOPNOTSUPP;
    }
    if data.is_null() || file.is_null() {
        return -EINVAL;
    }
    // SAFETY: `data` is confirmed non-null and caller guarantees it points to a valid, aligned DrmSyncobjTimelineArray buffer.
    let args = unsafe { &*data.cast::<DrmSyncobjTimelineArray>() };
    if args.flags & !DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED != 0 || args.count_handles == 0 {
        return -EINVAL;
    }
    let handles = match copy_handles(args.handles, args.count_handles) {
        Ok(handles) => handles,
        Err(error) => return error,
    };
    let objects = match find_syncobjs(file, &handles) {
        Ok(objects) => objects,
        Err(error) => return error,
    };
    if args.points == 0 {
        return -EFAULT;
    }
    for (index, object) in objects.iter().enumerate() {
        let point = chain_query_point(
            object.fence(),
            args.flags & DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED != 0,
        );
        let destination = (args.points as usize as *mut u64).wrapping_add(index);
        // SAFETY: `destination` points into user buffer `args.points` at `index`; copy_to_user validates user address and writes 8 bytes.
        if unsafe {
            copy_to_user(
                destination.cast(),
                (&point as *const u64).cast(),
                size_of::<u64>(),
            )
        } != 0
        {
            return -EFAULT;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use drm_kapi::config::{EINVAL, ENODEV, ENOMEM};

    use super::*;
    use crate::{
        drm_drv_types::DrmDriver,
        drm_drv_upstream::{drm_dev_alloc, drm_dev_put},
        drm_file_upstream::{drm_file_alloc, drm_file_free},
    };

    fn test_device() -> (*mut DrmDevice, alloc::boxed::Box<DrmDriver>) {
        crate::test_support::init_runtime();
        // SAFETY: `DrmDriver` is a C-compatible repr(C) driver struct; all fields are zero-initializable.
        let mut driver = alloc::boxed::Box::new(unsafe { core::mem::zeroed::<DrmDriver>() });
        driver.driver_features = DRIVER_SYNCOBJ;
        // SAFETY: `driver` is an allocated DrmDriver; parent device pointer is null.
        let dev = unsafe { drm_dev_alloc(&*driver, ptr::null_mut()) };
        assert!(!dev.is_null());
        (dev, driver)
    }

    fn syncobj_handle_ioctl(
        dev: *mut DrmDevice,
        file: *mut DrmFile,
        args: &mut DrmSyncobjHandle,
    ) -> i32 {
        // SAFETY: `dev`, `args`, and `file` are valid test harness pointers.
        unsafe { drm_syncobj_handle_to_fd_ioctl(dev, (args as *mut DrmSyncobjHandle).cast(), file) }
    }

    fn syncobj_fd_ioctl(
        dev: *mut DrmDevice,
        file: *mut DrmFile,
        args: &mut DrmSyncobjHandle,
    ) -> i32 {
        // SAFETY: `dev`, `args`, and `file` are valid test harness pointers.
        unsafe { drm_syncobj_fd_to_handle_ioctl(dev, (args as *mut DrmSyncobjHandle).cast(), file) }
    }

    fn assert_native_inline_fence_lock(fence: *mut DmaFence) {
        // SAFETY: `fence` is a valid, live software or chain fence initialized with inline lock storage.
        unsafe {
            assert_ne!(
                (*fence).flags & (1 << drm_kapi::dma_fence::DMA_FENCE_FLAG_INLINE_LOCK_BIT),
                0,
                "fence must select its inline lock"
            );
            let inline_lock = ptr::addr_of_mut!((*fence).lock.inline_lock);
            let selected_lock = drm_kapi::dma_fence::dma_fence_spinlock(fence);
            assert_eq!(selected_lock, inline_lock);
            let lock_word = core::sync::atomic::AtomicU32::from_ptr(selected_lock.cast::<u32>());
            assert_eq!(
                lock_word.load(Ordering::Acquire),
                0,
                "inline lock storage must contain the native unlocked value"
            );
        }
    }

    #[test]
    fn syncobj_software_and_chain_fences_initialize_native_inline_locks() {
        crate::test_support::init_runtime();

        let software = create_software_fence(false).expect("software fence allocation");
        assert_native_inline_fence_lock(software);
        // SAFETY: `software` is an allocated, live software fence.
        unsafe {
            drm_kapi::dma_fence::dma_fence_signal(software);
            assert!(drm_kapi::dma_fence::dma_fence_is_signaled(software));
            drm_kapi::dma_fence::dma_fence_put(software);
        }

        let child = create_software_fence(false).expect("chain child allocation");
        let chain = drm_syncobj_chain_alloc();
        assert!(!chain.is_null());
        // SAFETY: `chain` is allocated via drm_syncobj_chain_alloc; `child` is a live software fence.
        unsafe {
            assert_eq!(init_chain(chain, None, child, 1), 0);
            let base = ptr::addr_of_mut!((*chain).base);
            assert_native_inline_fence_lock(base);
            drm_kapi::dma_fence::dma_fence_signal(base);
            assert!(drm_kapi::dma_fence::dma_fence_is_signaled(base));
            drm_kapi::dma_fence::dma_fence_put(base);
            drm_kapi::dma_fence::dma_fence_put(child);
        }
    }

    #[test]
    fn timeline_signaled_point_is_waitable_and_query_returns_its_point() {
        let (dev, mut driver) = test_device();
        driver.driver_features |= DRIVER_SYNCOBJ_TIMELINE;
        // SAFETY: `dev` was allocated by test_device(); `file` is allocated via drm_file_alloc;
        // all ioctl arguments point to valid, aligned stack records.
        unsafe {
            // `drm_dev_alloc` snapshots the driver feature mask.
            (*dev).driver_features |= DRIVER_SYNCOBJ_TIMELINE;
            let file = drm_file_alloc((*dev).primary);
            assert!(!file.is_null());

            let mut create = DrmSyncobjCreate {
                handle: 0,
                flags: 0,
            };
            assert_eq!(
                drm_syncobj_create_ioctl(dev, (&mut create as *mut DrmSyncobjCreate).cast(), file,),
                0
            );

            let handle = create.handle;
            let point = 17u64;
            let mut wait = DrmSyncobjTimelineWait {
                handles: (&handle as *const u32) as u64,
                points: (&point as *const u64) as u64,
                timeout_nsec: 0,
                count_handles: 1,
                flags: 0,
                first_signaled: 0,
                pad: 0,
                deadline_nsec: 0,
            };
            assert_eq!(
                drm_syncobj_timeline_wait_ioctl(
                    dev,
                    (&mut wait as *mut DrmSyncobjTimelineWait).cast(),
                    file,
                ),
                -EINVAL,
                "an unsubmitted timeline point is not yet waitable"
            );

            let signal = DrmSyncobjTimelineArray {
                handles: (&handle as *const u32) as u64,
                points: (&point as *const u64) as u64,
                count_handles: 1,
                flags: 0,
            };
            assert_eq!(
                drm_syncobj_timeline_signal_ioctl(
                    dev,
                    (&signal as *const DrmSyncobjTimelineArray as *mut DrmSyncobjTimelineArray)
                        .cast(),
                    file,
                ),
                0
            );

            // Keep the ioctl's zero deadline: the signaled timeline chain
            // must be recognized as ready without sleeping.
            wait.flags = 0;
            assert_eq!(
                drm_syncobj_timeline_wait_ioctl(
                    dev,
                    (&mut wait as *mut DrmSyncobjTimelineWait).cast(),
                    file,
                ),
                0,
                "the signaled contained fence makes its timeline point ready"
            );

            let mut queried_point = 0u64;
            let query = DrmSyncobjTimelineArray {
                handles: (&handle as *const u32) as u64,
                points: (&mut queried_point as *mut u64) as u64,
                count_handles: 1,
                flags: 0,
            };
            assert_eq!(
                drm_syncobj_query_ioctl(
                    dev,
                    (&query as *const DrmSyncobjTimelineArray as *mut DrmSyncobjTimelineArray)
                        .cast(),
                    file,
                ),
                0
            );
            assert_eq!(queried_point, point);

            let mut destroy = DrmSyncobjDestroy { handle, pad: 0 };
            assert_eq!(
                drm_syncobj_destroy_ioctl(
                    dev,
                    (&mut destroy as *mut DrmSyncobjDestroy).cast(),
                    file,
                ),
                0
            );
            drm_file_free(file);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn syncobj_handle_fd_handle_round_trip_preserves_references_across_close() {
        let _serial = test_file_ops::serial();
        test_file_ops::install();

        let (dev, _driver) = test_device();
        // SAFETY: `dev` was allocated by test_device(); files are allocated via drm_file_alloc;
        // all ioctl structures point to valid, aligned stack variables.
        unsafe {
            let file1 = drm_file_alloc((*dev).primary);
            let file2 = drm_file_alloc((*dev).primary);
            let file3 = drm_file_alloc((*dev).primary);
            assert!(!file1.is_null() && !file2.is_null() && !file3.is_null());

            let mut create = DrmSyncobjCreate {
                handle: 0,
                flags: 0,
            };
            assert_eq!(
                drm_syncobj_create_ioctl(dev, (&mut create as *mut DrmSyncobjCreate).cast(), file1),
                0
            );
            let original = drm_syncobj_find(file1, create.handle);
            assert!(!original.is_null());

            let mut export = DrmSyncobjHandle {
                handle: create.handle,
                flags: 0,
                fd: -1,
                pad: 0,
                point: 0,
            };
            assert_eq!(syncobj_handle_ioctl(dev, file1, &mut export), 0);
            assert!(export.fd >= 0);

            let mut import = DrmSyncobjHandle {
                handle: 0,
                flags: 0,
                fd: export.fd,
                pad: 0,
                point: 0,
            };
            assert_eq!(syncobj_fd_ioctl(dev, file2, &mut import), 0);
            let imported = drm_syncobj_find(file2, import.handle);
            assert_eq!(imported, original);
            drm_syncobj_put(imported);

            // Destroying the source handle must not invalidate the exported
            // descriptor: it retains an independent syncobj reference.
            let mut destroy = DrmSyncobjDestroy {
                handle: create.handle,
                pad: 0,
            };
            assert_eq!(
                drm_syncobj_destroy_ioctl(
                    dev,
                    (&mut destroy as *mut DrmSyncobjDestroy).cast(),
                    file1,
                ),
                0
            );
            let mut import_again = DrmSyncobjHandle {
                handle: 0,
                flags: 0,
                fd: export.fd,
                pad: 0,
                point: 0,
            };
            assert_eq!(syncobj_fd_ioctl(dev, file3, &mut import_again), 0);
            let imported_again = drm_syncobj_find(file3, import_again.handle);
            assert_eq!(imported_again, original);
            drm_syncobj_put(imported_again);

            // Closing the descriptor drops only its own reference. Imported
            // handles continue to identify the same object and remain usable.
            test_file_ops::close_fd(export.fd);
            let mut closed_import = DrmSyncobjHandle {
                handle: 0,
                flags: 0,
                fd: export.fd,
                pad: 0,
                point: 0,
            };
            assert_eq!(syncobj_fd_ioctl(dev, file3, &mut closed_import), -EINVAL);
            let still_alive_file2 = drm_syncobj_find(file2, import.handle);
            assert_eq!(still_alive_file2, original);
            drm_syncobj_put(still_alive_file2);
            let still_alive = drm_syncobj_find(file3, import_again.handle);
            assert_eq!(still_alive, original);
            drm_syncobj_put(still_alive);
            drm_syncobj_put(original);

            for (file, handle) in [(file2, import.handle), (file3, import_again.handle)] {
                let mut destroy = DrmSyncobjDestroy { handle, pad: 0 };
                assert_eq!(
                    drm_syncobj_destroy_ioctl(
                        dev,
                        (&mut destroy as *mut DrmSyncobjDestroy).cast(),
                        file,
                    ),
                    0
                );
            }
            drm_file_free(file3);
            drm_file_free(file2);
            drm_file_free(file1);
            drm_dev_put(dev);
        }
    }

    #[test]
    fn syncobj_fd_conversion_rejects_invalid_inputs_and_unwinds_failures() {
        let _serial = test_file_ops::serial();
        test_file_ops::install();

        let (dev, _driver) = test_device();
        // SAFETY: `dev` was allocated by test_device(); `file` is allocated via drm_file_alloc;
        // all ioctl structures point to valid, aligned stack variables.
        unsafe {
            let file = drm_file_alloc((*dev).primary);
            assert!(!file.is_null());
            let mut create = DrmSyncobjCreate {
                handle: 0,
                flags: 0,
            };
            assert_eq!(
                drm_syncobj_create_ioctl(dev, (&mut create as *mut DrmSyncobjCreate).cast(), file),
                0
            );

            let mut invalid_handle = DrmSyncobjHandle {
                handle: create.handle + 10,
                flags: 0,
                fd: -1,
                pad: 0,
                point: 0,
            };
            assert_eq!(
                syncobj_handle_ioctl(dev, file, &mut invalid_handle),
                -EINVAL
            );

            let mut bad_flags = DrmSyncobjHandle {
                handle: create.handle,
                flags: 1 << 31,
                fd: -1,
                pad: 0,
                point: 0,
            };
            assert_eq!(syncobj_handle_ioctl(dev, file, &mut bad_flags), -EINVAL);
            let mut bad_pad = DrmSyncobjHandle {
                handle: create.handle,
                flags: 0,
                fd: -1,
                pad: 1,
                point: 0,
            };
            assert_eq!(syncobj_handle_ioctl(dev, file, &mut bad_pad), -EINVAL);

            let mut missing_fd = DrmSyncobjHandle {
                handle: 0,
                flags: 0,
                fd: i32::MAX,
                pad: 0,
                point: 0,
            };
            assert_eq!(syncobj_fd_ioctl(dev, file, &mut missing_fd), -EINVAL);

            let plain_fd = test_file_ops::install_plain_file();
            let mut wrong_file_type = DrmSyncobjHandle {
                handle: 0,
                flags: 0,
                fd: plain_fd,
                pad: 0,
                point: 0,
            };
            assert_eq!(syncobj_fd_ioctl(dev, file, &mut wrong_file_type), -EINVAL);
            test_file_ops::close_fd(plain_fd);

            let mut point_without_timeline = DrmSyncobjHandle {
                handle: create.handle,
                flags: 0,
                fd: -1,
                pad: 0,
                point: 1,
            };
            assert_eq!(
                syncobj_handle_ioctl(dev, file, &mut point_without_timeline),
                -EINVAL
            );

            let put_count = test_file_ops::put_unused_count();
            test_file_ops::fail_next_fd(-ENODEV);
            let mut no_descriptor = DrmSyncobjHandle {
                handle: create.handle,
                flags: 0,
                fd: -1,
                pad: 0,
                point: 0,
            };
            assert_eq!(syncobj_handle_ioctl(dev, file, &mut no_descriptor), -ENODEV);
            assert_eq!(test_file_ops::put_unused_count(), put_count);

            test_file_ops::fail_next_anon_inode(-ENOMEM);
            let mut no_file = DrmSyncobjHandle {
                handle: create.handle,
                flags: 0,
                fd: -1,
                pad: 0,
                point: 0,
            };
            assert_eq!(syncobj_handle_ioctl(dev, file, &mut no_file), -ENOMEM);
            assert_eq!(test_file_ops::put_unused_count(), put_count + 1);

            let mut destroy = DrmSyncobjDestroy {
                handle: create.handle,
                pad: 0,
            };
            assert_eq!(
                drm_syncobj_destroy_ioctl(
                    dev,
                    (&mut destroy as *mut DrmSyncobjDestroy).cast(),
                    file,
                ),
                0
            );
            drm_file_free(file);
            drm_dev_put(dev);
        }
    }
}
