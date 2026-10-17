// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Asynchronous Linux `work_struct` dispatch for the LinuxKPI boundary.
//!
//! This adapter provides a pool of persistent task-context workers fed by one
//! intrusive FIFOs for ordinary and IRQ work. Producers only publish pending
//! work under an IRQ-safe lock; callbacks are always invoked later by a worker,
//! never inline.
//!
//! The pool keeps the Linux execution guarantees the DRM ports rely
//! on:
//! - a work item never runs on two workers at once (non-reentrancy); an item
//!   requeued while running waits for that instance to finish;
//! - each `workqueue_struct` limits its running items to `max_active`, so an
//!   ordered queue (`__WQ_ORDERED`, `max_active == 1`) runs its items one at a
//!   time in queue order;
//! - a work item blocking on another item does not starve it: a manager task
//!   adds workers (up to [`MAX_WORKERS`]) when runnable work waits while no
//!   worker is idle, standing in for Linux's concurrency-managed pools.
//!
//! Workqueue priority (`WQ_HIGHPRI`) and CPU affinity are not modelled.
//! The executor must be initialized from blockable task context before work
//! can run; items queued earlier wait on the FIFO.

#![allow(non_snake_case)]

use alloc::{boxed::Box, string::String};
use core::{
    ffi::c_void,
    fmt::Write as _,
    hint::spin_loop,
    ops::{Deref, DerefMut},
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicUsize, Ordering},
};

use crate::{
    kernel_services,
    locks::NoPreemptIrqSave,
    timer::{del_timer, del_timer_sync, mod_timer, timer_setup},
    types::{DelayedWork, IrqWork, IrqWorkNode, ListHead, TimerList, WorkStruct},
};

const WORK_STRUCT_PENDING_BIT: u32 = 0;

/// Linux `work_pending()` tests WORK_STRUCT_PENDING_BIT with READ_ONCE.
///
/// # Safety
/// `work` must point to a live, properly aligned `WorkStruct` whose `data`
/// field is initialized. Keep the object allocated for the duration of this
/// read; concurrent workqueue updates are permitted.
#[inline]
pub unsafe fn work_pending(work: *const WorkStruct) -> bool {
    // SAFETY: Caller guarantees `work` points to a live, properly aligned `WorkStruct`
    // whose `data` field is initialized and remains allocated during this read.
    let data = unsafe { ptr::read_volatile(ptr::addr_of!((*work).data)) };
    data & (1u64 << WORK_STRUCT_PENDING_BIT) != 0
}

const WORKER_UNINITIALIZED: u8 = 0;
const WORKER_STARTING: u8 = 1;
const WORKER_READY: u8 = 2;

// Linux's WORK_STRUCT_PENDING_BIT is bit 0. With CONFIG_DEBUG_OBJECTS_WORK
// unset and 64-bit unsigned long, the Linux 7.2.3 WORK_DATA_INIT() off-queue
// sentinel is ((1 << 31) - 1) << 21.
const WORK_PENDING: usize = 1 << 0;
const WORK_DATA_INIT: usize = ((1usize << 31) - 1) << 21;

const _: [(); core::mem::size_of::<usize>()] = [(); core::mem::size_of::<core::ffi::c_ulong>()];
const _: [(); 32] = [(); core::mem::size_of::<WorkStruct>()];
const _: [(); 8] = [(); core::mem::align_of::<WorkStruct>()];
const _: [(); 0] = [(); core::mem::offset_of!(WorkStruct, data)];
const _: [(); 8] = [(); core::mem::offset_of!(WorkStruct, entry)];
const _: [(); 24] = [(); core::mem::offset_of!(WorkStruct, function)];
const _: [(); 40] = [(); core::mem::size_of::<TimerList>()];
const _: [(); 8] = [(); core::mem::align_of::<TimerList>()];
const _: [(); 0] = [(); core::mem::offset_of!(TimerList, entry)];
const _: [(); 16] = [(); core::mem::offset_of!(TimerList, expires)];
const _: [(); 24] = [(); core::mem::offset_of!(TimerList, function)];
const _: [(); 32] = [(); core::mem::offset_of!(TimerList, flags)];
const _: [(); 88] = [(); core::mem::size_of::<DelayedWork>()];
const _: [(); 32] = [(); core::mem::offset_of!(DelayedWork, timer)];
const _: [(); 72] = [(); core::mem::offset_of!(DelayedWork, workqueue)];
const _: [(); 80] = [(); core::mem::offset_of!(DelayedWork, cpu)];

/// Linux `WQ_MAX_ACTIVE` and `WQ_DFL_ACTIVE` (include/linux/workqueue.h).
const WQ_MAX_ACTIVE: u32 = 2048;
const WQ_DFL_ACTIVE: u32 = WQ_MAX_ACTIVE / 2;
/// Linux `__WQ_ORDERED` (include/linux/workqueue.h).
const __WQ_ORDERED: u32 = 1 << 17;
const WORKQUEUE_MAGIC: u32 = 0x5751_5545;

/// Linux `workqueue_struct`. Every queue feeds the shared worker pool; the
/// handle carries its concurrency limit and the in-flight count that
/// `flush_workqueue()` waits on. The counters change only under `WORK_QUEUE`.
#[repr(C)]
pub struct WorkqueueStruct {
    magic: u32,
    max_active: u32,
    /// Items of this queue currently executing.
    active: AtomicU32,
    /// Items of this queue queued or executing.
    inflight: AtomicUsize,
}

impl WorkqueueStruct {
    const fn new(max_active: u32) -> Self {
        Self {
            magic: WORKQUEUE_MAGIC,
            max_active,
            active: AtomicU32::new(0),
            inflight: AtomicUsize::new(0),
        }
    }
}

// Queued items keep their workqueue pointer in `work_struct.data` next to the
// PENDING bit, as Linux keeps its pool_workqueue there.
const _: () = assert!(core::mem::align_of::<WorkqueueStruct>() > WORK_PENDING);

static SYSTEM_DEFAULT_WORKQUEUE: WorkqueueStruct = WorkqueueStruct::new(WQ_DFL_ACTIVE);
static SYSTEM_HIGHPRI_WORKQUEUE: WorkqueueStruct = WorkqueueStruct::new(WQ_DFL_ACTIVE);

#[allow(non_upper_case_globals)]
pub const system_dfl_wq: *mut WorkqueueStruct =
    core::ptr::addr_of!(SYSTEM_DEFAULT_WORKQUEUE).cast_mut();
#[allow(non_upper_case_globals)]
pub const system_highpri_wq: *mut WorkqueueStruct =
    core::ptr::addr_of!(SYSTEM_HIGHPRI_WORKQUEUE).cast_mut();

/// A workqueue pointer published as a C data symbol.
#[repr(transparent)]
pub struct ExportedWorkqueue(*mut WorkqueueStruct);
// SAFETY: `ExportedWorkqueue` wraps a pointer to an immutable static `WorkqueueStruct` (`system_dfl_wq`).
// Its internal fields are synchronized via atomic variables and `WORK_QUEUE`, making shared concurrent access safe.
unsafe impl Sync for ExportedWorkqueue {}

/// Linux `struct workqueue_struct *system_dfl_wq` (kernel/workqueue.c), read
/// by C-ABI callers such as DRM drivers.
#[unsafe(export_name = "system_dfl_wq")]
pub static SYSTEM_DFL_WQ_SYMBOL: ExportedWorkqueue = ExportedWorkqueue(system_dfl_wq);

/// Failure from an operation which needs the asynchronous work runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkqueueError {
    /// No task-context worker could be published.
    WorkerUnavailable,
    /// A blocking operation was attempted from a context that cannot block.
    ContextCannotBlock,
    /// The native wait provider could not block the caller.
    WaitFailed,
    /// The caller supplied a null work item or workqueue handle.
    NullPointer,
    /// The work item was queued before `INIT_WORK` installed its callback.
    WorkNotInitialized,
}

/// Conversion for translated C workqueue-pointer arguments.
pub trait WorkqueuePtr {
    fn workqueue_ptr(self) -> *const c_void;
}

impl<T> WorkqueuePtr for *mut T {
    fn workqueue_ptr(self) -> *const c_void {
        self.cast()
    }
}

impl<T> WorkqueuePtr for *const T {
    fn workqueue_ptr(self) -> *const c_void {
        self.cast()
    }
}

/// Conversion for the reference and pointer forms used by source-order
/// workqueue helpers. Queueing retains the pointer asynchronously, so callers
/// must keep the backing work item alive until it is flushed or cancelled.
pub trait WorkStructPtr {
    fn work_struct_ptr(self) -> *mut WorkStruct;
}

impl WorkStructPtr for *mut WorkStruct {
    fn work_struct_ptr(self) -> *mut WorkStruct {
        self
    }
}

impl WorkStructPtr for *const WorkStruct {
    fn work_struct_ptr(self) -> *mut WorkStruct {
        self.cast_mut()
    }
}

impl WorkStructPtr for &mut WorkStruct {
    fn work_struct_ptr(self) -> *mut WorkStruct {
        self
    }
}

impl WorkStructPtr for &WorkStruct {
    fn work_struct_ptr(self) -> *mut WorkStruct {
        (self as *const WorkStruct).cast_mut()
    }
}

pub trait DelayedWorkPtr {
    fn delayed_work_ptr(self) -> *mut DelayedWork;
}
impl DelayedWorkPtr for *mut DelayedWork {
    fn delayed_work_ptr(self) -> *mut DelayedWork {
        self
    }
}
impl DelayedWorkPtr for *const DelayedWork {
    fn delayed_work_ptr(self) -> *mut DelayedWork {
        self.cast_mut()
    }
}
impl DelayedWorkPtr for &mut DelayedWork {
    fn delayed_work_ptr(self) -> *mut DelayedWork {
        self
    }
}
impl DelayedWorkPtr for &DelayedWork {
    fn delayed_work_ptr(self) -> *mut DelayedWork {
        self as *const DelayedWork as *mut DelayedWork
    }
}

/// Return an atomic reference to the embedded workqueue pointer within `work`.
///
/// # Safety
/// `work` must point to a live, properly aligned `DelayedWork` whose `workqueue` field
/// is valid for the lifetime `'a`.
unsafe fn delayed_workqueue_atomic<'a>(work: *mut DelayedWork) -> &'a AtomicPtr<c_void> {
    assert!(!work.is_null());
    // SAFETY: Caller guarantees `work` points to a live, properly aligned `DelayedWork`.
    let slot = unsafe { ptr::addr_of_mut!((*work).workqueue) };
    // SAFETY: `DelayedWork.workqueue` is naturally aligned pointer storage;
    // all accesses by this compatibility layer use this atomic view.
    unsafe { AtomicPtr::from_ptr(slot) }
}

struct WorkList {
    head: *mut ListHead,
    tail: *mut ListHead,
    irq_head: *mut IrqWorkNode,
    irq_tail: *mut IrqWorkNode,
}

impl WorkList {
    const fn new() -> Self {
        Self {
            head: ptr::null_mut(),
            tail: ptr::null_mut(),
            irq_head: ptr::null_mut(),
            irq_tail: ptr::null_mut(),
        }
    }

    /// The caller holds `WORK_QUEUE` and the node is self-linked.
    ///
    /// # Safety
    /// `work` must point to a live, properly aligned `WorkStruct` whose `entry`
    /// is currently self-linked. The caller must hold `WORK_QUEUE` ensuring exclusive access.
    unsafe fn push(&mut self, work: *mut WorkStruct) {
        // SAFETY: Caller guarantees `work` is non-null, aligned, and points to a live `WorkStruct`.
        let node = unsafe { ptr::addr_of_mut!((*work).entry) };
        // SAFETY: `node` points to the valid `entry` field within `work`.
        unsafe {
            assert!(core::ptr::eq((*node).next, node));
            assert!(core::ptr::eq((*node).prev, node));
        }
        if self.tail.is_null() {
            self.head = node;
            self.tail = node;
            // SAFETY: `node` points to the valid `entry` field within `work`.
            unsafe {
                (*node).next = node;
                (*node).prev = node;
            }
        } else {
            // SAFETY: `node`, `self.head`, and `self.tail` point to valid `ListHead` nodes under `WORK_QUEUE`.
            unsafe {
                (*node).next = self.head;
                (*node).prev = self.tail;
                (*self.tail).next = node;
                (*self.head).prev = node;
            }
            self.tail = node;
        }
    }

    /// Remove a pending item. The caller holds `WORK_QUEUE`.
    ///
    /// # Safety
    /// `work` must point to a live, properly aligned `WorkStruct`. The caller must
    /// hold `WORK_QUEUE` ensuring exclusive access.
    unsafe fn remove(&mut self, work: *mut WorkStruct) -> bool {
        // SAFETY: Caller guarantees `work` is non-null, aligned, and points to a live `WorkStruct`.
        let node = unsafe { ptr::addr_of_mut!((*work).entry) };
        // SAFETY: `node` points to the `entry` field within `work`.
        let next = unsafe { (*node).next };
        if self.head.is_null() || (next == node && self.head != node) {
            return false;
        }
        // SAFETY: `node` points to the `entry` field within `work`.
        let previous = unsafe { (*node).prev };
        if next == node {
            self.head = ptr::null_mut();
            self.tail = ptr::null_mut();
        } else {
            // SAFETY: `previous` and `next` point to valid list nodes under `WORK_QUEUE`.
            unsafe {
                (*previous).next = next;
                (*next).prev = previous;
            }
            if self.head == node {
                self.head = next;
            }
            if self.tail == node {
                self.tail = previous;
            }
        }
        // SAFETY: `node` points to the `entry` field within `work`.
        unsafe {
            (*node).next = node;
            (*node).prev = node;
        }
        true
    }

    /// Append an IRQ work node to the deferred FIFO. The caller holds
    /// `WORK_QUEUE`; the node must not be linked in another queue.
    ///
    /// # Safety
    /// `work` must point to a live `IrqWork` whose `node` is unlinked (`next` is null).
    /// The caller must hold `WORK_QUEUE`.
    unsafe fn push_irq(&mut self, work: *mut IrqWork) {
        // SAFETY: Caller guarantees `work` points to a live `IrqWork`.
        let node = unsafe { ptr::addr_of_mut!((*work).node) };
        // SAFETY: `node` points to the `node` field within `work`.
        assert!(unsafe { (*node).next.is_null() });
        if self.irq_tail.is_null() {
            self.irq_head = node;
        } else {
            // SAFETY: `self.irq_tail` points to a valid queued node under `WORK_QUEUE`.
            unsafe { (*self.irq_tail).next = node };
        }
        self.irq_tail = node;
    }

    /// Remove the oldest IRQ work node. The caller holds `WORK_QUEUE`.
    ///
    /// # Safety
    /// The caller must hold `WORK_QUEUE`.
    unsafe fn pop_irq(&mut self) -> *mut IrqWork {
        let node = self.irq_head;
        if node.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `node` is non-null and points to a queued `IrqWorkNode` under `WORK_QUEUE`.
        self.irq_head = unsafe { (*node).next };
        if self.irq_head.is_null() {
            self.irq_tail = ptr::null_mut();
        }
        // SAFETY: `node` points to the dequeued `IrqWorkNode`.
        unsafe { (*node).next = ptr::null_mut() };
        // SAFETY: `node` was embedded in a live `IrqWork` struct at `offset_of!(IrqWork, node)`.
        unsafe {
            node.cast::<u8>()
                .sub(core::mem::offset_of!(IrqWork, node))
                .cast()
        }
    }
}

/// Obtain the enclosing `WorkStruct` pointer from an embedded `entry` node.
///
/// # Safety
/// `node` must point to the `entry` field of a valid `WorkStruct`.
unsafe fn work_from_entry(node: *mut ListHead) -> *mut WorkStruct {
    // SAFETY: Caller guarantees `node` is embedded inside a live `WorkStruct` at `offset_of!(WorkStruct, entry)`.
    unsafe {
        node.cast::<u8>()
            .sub(core::mem::offset_of!(WorkStruct, entry))
            .cast()
    }
}

// Queue pointers are accessed only while WORK_QUEUE is locked, and the caller
// owns each work item through its flush/cancel lifetime.
// SAFETY: `WorkList` contains intrusive pointers (`ListHead`, `IrqWorkNode`). It is `Send` because access
// is strictly synchronized by `IrqSpinLock<WorkList>` (`WORK_QUEUE`), and the list owns its head/tail pointers.
unsafe impl Send for WorkList {}

struct IrqSpinLock<T> {
    held: AtomicBool,
    data: core::cell::UnsafeCell<T>,
}

// SAFETY: `IrqSpinLock` synchronizes access to `data` via an atomic spinlock with interrupts and preemption
// disabled. If `T: Send`, transferring ownership across threads is safe.
unsafe impl<T: Send> Send for IrqSpinLock<T> {}
// SAFETY: `IrqSpinLock` ensures mutually exclusive mutable access to `T` via `IrqSpinGuard`. Multiple threads
// can share `IrqSpinLock<T>` safely.
unsafe impl<T: Send> Sync for IrqSpinLock<T> {}

impl<T> IrqSpinLock<T> {
    const fn new(data: T) -> Self {
        Self {
            held: AtomicBool::new(false),
            data: core::cell::UnsafeCell::new(data),
        }
    }

    fn lock(&self) -> IrqSpinGuard<'_, T> {
        let irq_guard = NoPreemptIrqSave::new();
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Test-and-test-and-set: the Acquire CAS is the synchronization point;
            // this poll only waits for the holder's Release store before retrying.
            // sync-lint: ignore suspicious_relaxed_wait_condition
            // sync-lint: ignore suspicious_relaxed_mixed_ordering
            while self.held.load(Ordering::Relaxed) {
                spin_loop();
            }
        }
        IrqSpinGuard {
            lock: self,
            _irq_guard: irq_guard,
        }
    }
}

struct IrqSpinGuard<'a, T> {
    lock: &'a IrqSpinLock<T>,
    _irq_guard: NoPreemptIrqSave,
}

impl<T> Deref for IrqSpinGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The spinlock was acquired with Acquire ordering, guaranteeing exclusive access
        // to `data` while this guard exists.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for IrqSpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: The spinlock was acquired with Acquire ordering, and `&mut self` guarantees
        // exclusive mutable access to `data`.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for IrqSpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.held.store(false, Ordering::Release);
    }
}

static WORK_QUEUE: IrqSpinLock<WorkList> = IrqSpinLock::new(WorkList::new());
static WORKER_STATE: AtomicU8 = AtomicU8::new(WORKER_UNINITIALIZED);
static WORK_GENERATION: AtomicUsize = AtomicUsize::new(0);
/// Items on the FIFO (pending, not yet running). Changed under `WORK_QUEUE`.
static QUEUED: AtomicUsize = AtomicUsize::new(0);
/// IRQ work on the shared deferred FIFO (pending, not yet running).
static IRQ_QUEUED: AtomicUsize = AtomicUsize::new(0);
/// Bound uninterrupted IRQ-work execution so ordinary work cannot starve.
const MAX_IRQ_WORK_BURST: usize = 8;

/// Upper bound on pool workers. Each one is a kernel task with its own stack.
pub const MAX_WORKERS: usize = 32;
const INITIAL_WORKERS: usize = 2;
/// How long runnable work may wait with no idle worker before the manager
/// adds one: long enough that ordinary busy workers pick it up first.
const STARVATION_GRACE_NS: u64 = 10_000_000;

struct WorkerSlot {
    task: AtomicUsize,
    current: AtomicPtr<WorkStruct>,
    current_wq: AtomicPtr<WorkqueueStruct>,
}

impl WorkerSlot {
    const fn new() -> Self {
        Self {
            task: AtomicUsize::new(0),
            current: AtomicPtr::new(ptr::null_mut()),
            current_wq: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

static WORKERS: [WorkerSlot; MAX_WORKERS] = [const { WorkerSlot::new() }; MAX_WORKERS];
/// Claimed prefix of `WORKERS`.
static NR_WORKERS: AtomicUsize = AtomicUsize::new(0);
static IDLE_WORKERS: AtomicUsize = AtomicUsize::new(0);

fn workers() -> &'static [WorkerSlot] {
    &WORKERS[..NR_WORKERS.load(Ordering::Acquire).min(MAX_WORKERS)]
}

fn notify_workers() {
    WORK_GENERATION.fetch_add(1, Ordering::Release);
    crate::wait::wake_all();
}

/// Wake the manager when work is queued and every worker is occupied.
fn kick_manager_if_starved() {
    if (QUEUED.load(Ordering::Acquire) != 0 || IRQ_QUEUED.load(Ordering::Acquire) != 0)
        && IDLE_WORKERS.load(Ordering::Acquire) == 0
    {
        crate::wait::wake_all();
    }
}

fn make_task_name(prefix: &str, index: Option<usize>) -> Result<String, WorkqueueError> {
    let mut name = String::new();
    name.try_reserve_exact(24)
        .map_err(|_| WorkqueueError::WorkerUnavailable)?;
    name.push_str(prefix);
    if let Some(index) = index {
        let _ = write!(name, "/{index}");
    }
    Ok(name)
}

/// Claim the next worker slot and start its task. Called only by the
/// executor initializer and by the single manager task.
fn spawn_worker() -> Result<(), WorkqueueError> {
    let index = NR_WORKERS.load(Ordering::Acquire);
    if index >= MAX_WORKERS {
        return Err(WorkqueueError::WorkerUnavailable);
    }
    let name = make_task_name("kworker", Some(index))?;
    crate::task_runtime::spawn(name, Box::new(move || workqueue_worker(index)))
        .map_err(|_| WorkqueueError::WorkerUnavailable)?;
    NR_WORKERS.store(index + 1, Ordering::Release);
    Ok(())
}

/// Start the persistent worker pool from blockable task context.
/// This is idempotent and must complete before queued work can run.
pub fn init_workqueue_executor() -> Result<(), WorkqueueError> {
    if !crate::task_runtime::can_block_current() {
        return Err(WorkqueueError::ContextCannotBlock);
    }

    loop {
        match WORKER_STATE.load(Ordering::Acquire) {
            WORKER_READY => return Ok(()),
            WORKER_STARTING => {
                let ready = || WORKER_STATE.load(Ordering::Acquire) != WORKER_STARTING;
                if !crate::wait::wait_until_ready(None, &ready) {
                    return Err(WorkqueueError::WaitFailed);
                }
            }
            WORKER_UNINITIALIZED => {
                if WORKER_STATE
                    .compare_exchange(
                        WORKER_UNINITIALIZED,
                        WORKER_STARTING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    continue;
                }

                let started = (|| {
                    while NR_WORKERS.load(Ordering::Acquire) < INITIAL_WORKERS {
                        spawn_worker()?;
                    }
                    let name = make_task_name("kworker-manager", None)?;
                    crate::task_runtime::spawn(name, Box::new(workqueue_manager))
                        .map_err(|_| WorkqueueError::WorkerUnavailable)?;
                    Ok(())
                })();
                if let Err(error) = started {
                    // Workers already spawned stay parked until a later
                    // initializer publishes READY.
                    WORKER_STATE.store(WORKER_UNINITIALIZED, Ordering::Release);
                    crate::wait::wake_all();
                    return Err(error);
                }
                WORKER_STATE.store(WORKER_READY, Ordering::Release);
                notify_workers();
                return Ok(());
            }
            _ => unreachable!("invalid workqueue worker state"),
        }
    }
}

/// Obtain an atomic reference to the `data` field of a `WorkStruct`.
///
/// # Safety
/// `work` must point to a live, properly aligned `WorkStruct` valid for `'a`.
unsafe fn work_data<'a>(work: *mut WorkStruct) -> &'a AtomicUsize {
    assert!(!work.is_null());
    // Linux declares work_struct.data as atomic_long_t. This x86_64 binding
    // keeps the same storage size and alignment in its C-layout WorkStruct.
    // SAFETY: Caller guarantees `work` points to a live, properly aligned `WorkStruct`.
    // The `data` field has native `usize` size and alignment.
    unsafe { &*ptr::addr_of_mut!((*work).data).cast::<AtomicUsize>() }
}

/// Initialize `work_struct` for a non-capturing safe Rust callback taking
/// `&mut WorkStruct`. Function items and non-capturing closures are zero-sized;
/// capturing closures are rejected rather than retaining borrowed state.
pub fn INIT_WORK<F>(work: &mut WorkStruct, function: F)
where
    F: for<'a> Fn(&'a mut WorkStruct) + Copy + Send + 'static,
{
    let _ = init_workqueue_executor();
    assert_eq!(
        core::mem::size_of::<F>(),
        0,
        "INIT_WORK requires a non-capturing callback"
    );
    let _ = function;
    work.data = WORK_DATA_INIT as core::ffi::c_ulong;
    let entry = ptr::addr_of_mut!(work.entry);
    work.entry.next = entry;
    work.entry.prev = entry;
    work.function = Some(rust_ref_work_trampoline::<F>);
}

/// Trampoline invoking a zero-sized Rust closure callback.
///
/// # Safety
/// `work` must point to a live `WorkStruct`. `F` must be a zero-sized, non-capturing function/closure.
unsafe extern "C" fn rust_ref_work_trampoline<F>(work: *mut WorkStruct)
where
    F: for<'a> Fn(&'a mut WorkStruct) + Copy + Send + 'static,
{
    assert!(!work.is_null());
    // The only zero-sized Fn values accepted by INIT_WORK are function items
    // and non-capturing closures, whose value has no runtime storage.
    // SAFETY: `F` is constrained to zero-sized non-capturing types, so zeroed uninit is safe to assume_init.
    let function = unsafe { core::mem::MaybeUninit::<F>::zeroed().assume_init() };
    // SAFETY: Caller guarantees `work` is non-null, aligned, and valid during callback dispatch.
    function(unsafe { &mut *work });
}

/// Initialize a work item with an actual C-ABI callback.
pub fn INIT_WORK_C(work: &mut WorkStruct, function: unsafe extern "C" fn(*mut WorkStruct)) {
    let _ = init_workqueue_executor();
    work.data = WORK_DATA_INIT as core::ffi::c_ulong;
    let entry = ptr::addr_of_mut!(work.entry);
    work.entry.next = entry;
    work.entry.prev = entry;
    work.function = Some(function);
}

/// Callback invoked when a delayed work timer expires.
///
/// # Safety
/// `timer` must point to the `timer` field embedded within a live `DelayedWork`.
unsafe extern "C" fn delayed_work_timer_fn(timer: *mut TimerList) {
    assert!(!timer.is_null());
    // SAFETY: Caller guarantees `timer` is the embedded `timer` field in `DelayedWork` at offset `offset_of!(DelayedWork, timer)`.
    let delayed = unsafe {
        timer
            .cast::<u8>()
            .sub(core::mem::offset_of!(DelayedWork, timer))
            .cast::<DelayedWork>()
    };
    // SAFETY: `delayed` points to a live `DelayedWork`.
    let workqueue = unsafe { delayed_workqueue_atomic(delayed) }.load(Ordering::Acquire);
    assert!(!workqueue.is_null(), "delayed work timer has no workqueue");
    // SAFETY: `workqueue` is a valid workqueue pointer and `delayed` points to a live `DelayedWork`.
    unsafe { queue_work(workqueue, ptr::addr_of_mut!((*delayed).work)) };
}

/// Initialize Linux `delayed_work`: work callback plus a timer-list callback.
/// The timer backend is monotonic and asynchronous; delay units are jiffies.
pub fn INIT_DELAYED_WORK<F>(delayed: &mut DelayedWork, function: F)
where
    F: for<'a> Fn(&'a mut WorkStruct) + Copy + Send + 'static,
{
    INIT_WORK(&mut delayed.work, function);
    // SAFETY: `delayed` is a live `DelayedWork` reference.
    unsafe { delayed_workqueue_atomic(delayed) }.store(ptr::null_mut(), Ordering::Release);
    delayed.cpu = -1;
    // SAFETY: `delayed.timer` is a live `TimerList` field being initialized.
    unsafe { timer_setup(&mut delayed.timer, delayed_work_timer_fn, 0) };
}

/// Queue a delayed work item at an absolute jiffies delay.
///
/// # Safety
/// `workqueue` must identify a live workqueue and `delayed` must point to a
/// live `DelayedWork` initialized by `INIT_DELAYED_WORK`. Keep its storage and
/// callback state alive until `cancel_delayed_work_sync` or
/// `flush_delayed_work` completes; do not modify its embedded timer, work
/// links, or callback while it is queued or running.
pub unsafe fn queue_delayed_work<Q: WorkqueuePtr, D: DelayedWorkPtr>(
    workqueue: Q,
    delayed: D,
    delay_jiffies: u64,
) -> bool {
    let workqueue = workqueue.workqueue_ptr();
    let delayed = delayed.delayed_work_ptr();
    assert!(!workqueue.is_null() && !delayed.is_null());
    // SAFETY: `delayed` points to a live `DelayedWork`.
    if work_busy(unsafe { ptr::addr_of_mut!((*delayed).work) }) {
        return false;
    }
    // SAFETY: `delayed` points to a live `DelayedWork`.
    unsafe { delayed_workqueue_atomic(delayed) }.store(workqueue.cast_mut(), Ordering::Release);
    if delay_jiffies == 0 {
        // SAFETY: Caller guarantees `workqueue` and `delayed` satisfy `queue_work` preconditions.
        return unsafe { queue_work(workqueue, ptr::addr_of_mut!((*delayed).work)) };
    }
    let expires = crate::primitives::jiffies().wrapping_add(delay_jiffies);
    // SAFETY: `delayed` points to a live `DelayedWork` whose embedded `timer` is valid.
    !unsafe {
        mod_timer(
            ptr::addr_of_mut!((*delayed).timer),
            expires as core::ffi::c_ulong,
        )
    }
}

/// Modify a delayed work deadline. Existing timer or work state is preserved
/// according to Linux's `mod_delayed_work()` contract.
///
/// # Safety
/// `workqueue` must identify a live workqueue and `delayed` must point to a
/// live `DelayedWork` initialized by `INIT_DELAYED_WORK`. Keep its storage and
/// callback state alive until `cancel_delayed_work_sync` or
/// `flush_delayed_work` completes; do not modify its embedded timer, work
/// links, or callback while it is queued or running.
pub unsafe fn mod_delayed_work<Q: WorkqueuePtr, D: DelayedWorkPtr>(
    workqueue: Q,
    delayed: D,
    delay_jiffies: u64,
) -> bool {
    let workqueue = workqueue.workqueue_ptr();
    let delayed = delayed.delayed_work_ptr();
    assert!(!workqueue.is_null() && !delayed.is_null());
    // SAFETY: `delayed` points to a live `DelayedWork`.
    unsafe { delayed_workqueue_atomic(delayed) }.store(workqueue.cast_mut(), Ordering::Release);
    if delay_jiffies == 0 {
        // SAFETY: `delayed` points to a live `DelayedWork` whose embedded `timer` is valid.
        let was_pending = unsafe { del_timer(ptr::addr_of_mut!((*delayed).timer)) };
        // SAFETY: Caller guarantees `workqueue` and `delayed` satisfy `queue_work` preconditions.
        return unsafe { queue_work(workqueue, ptr::addr_of_mut!((*delayed).work)) } || was_pending;
    }
    // SAFETY: `delayed` points to a live `DelayedWork` whose embedded `timer` is valid.
    unsafe {
        mod_timer(
            ptr::addr_of_mut!((*delayed).timer),
            crate::primitives::jiffies().wrapping_add(delay_jiffies) as core::ffi::c_ulong,
        )
    }
}

/// Cancel a queued timer and/or work item without waiting for a callback.
///
/// # Safety
/// `delayed` must point to a live `DelayedWork` initialized by
/// `INIT_DELAYED_WORK`. This is non-synchronous: retain the object and callback
/// state until a running callback has returned, using
/// `cancel_delayed_work_sync` when that guarantee is needed.
pub unsafe fn cancel_delayed_work<D: DelayedWorkPtr>(delayed: D) -> bool {
    let delayed = delayed.delayed_work_ptr();
    assert!(!delayed.is_null());
    // SAFETY: `delayed` points to a live `DelayedWork` whose embedded `timer` is valid.
    let timer_removed = unsafe { del_timer(ptr::addr_of_mut!((*delayed).timer)) };
    // SAFETY: `delayed` points to a live `DelayedWork` whose embedded `work` is valid.
    let work_removed = unsafe { cancel_work(ptr::addr_of_mut!((*delayed).work)) };
    timer_removed || work_removed
}

/// Synchronously cancel both the timer and any currently executing work.
///
/// # Safety
/// `delayed` must point to a live `DelayedWork` initialized by
/// `INIT_DELAYED_WORK` and remain allocated through this call. The caller must
/// be in blockable task context and must not cancel this work from its own
/// callback.
pub unsafe fn cancel_delayed_work_sync<D: DelayedWorkPtr>(delayed: D) -> bool {
    let delayed = delayed.delayed_work_ptr();
    assert!(!delayed.is_null());
    // SAFETY: `delayed` points to a live `DelayedWork` and caller is in blockable task context.
    let timer_removed = unsafe { del_timer_sync(ptr::addr_of_mut!((*delayed).timer)) };
    // SAFETY: `delayed` points to a live `DelayedWork` and caller is in blockable task context.
    let work_removed = unsafe { cancel_work_sync(ptr::addr_of_mut!((*delayed).work)) };
    timer_removed || work_removed
}

/// Force delayed work to become immediate, then wait for its callback.
///
/// # Safety
/// `delayed` must point to a live initialized `DelayedWork` and remain
/// allocated through the wait. The caller must be in blockable task context
/// and must not flush the work from its own callback.
pub unsafe fn flush_delayed_work<D: DelayedWorkPtr>(delayed: D) -> bool {
    let delayed = delayed.delayed_work_ptr();
    assert!(!delayed.is_null());
    // SAFETY: `delayed` points to a live `DelayedWork` and caller is in blockable task context.
    let timer_removed = unsafe { del_timer_sync(ptr::addr_of_mut!((*delayed).timer)) };
    if timer_removed {
        // SAFETY: `delayed` points to a live `DelayedWork`.
        let workqueue = unsafe { delayed_workqueue_atomic(delayed) }.load(Ordering::Acquire);
        assert!(!workqueue.is_null());
        // SAFETY: `workqueue` is valid and `delayed` points to a live `DelayedWork`.
        let _ = unsafe { queue_work(workqueue, ptr::addr_of_mut!((*delayed).work)) };
    }
    // SAFETY: `delayed` points to a live `DelayedWork` and caller is in blockable task context.
    let flushed = unsafe { flush_work(ptr::addr_of_mut!((*delayed).work)) };
    timer_removed || flushed
}

/// Queue one work instance. The callback never executes on this call's stack.
///
/// # Safety
/// The work item must remain allocated and exclusively owned by this workqueue
/// until `flush_work` or `cancel_work_sync` observes it idle. The caller must
/// initialize it first and must not modify its list node or callback while
/// queued/running.
pub unsafe fn queue_work<Q: WorkqueuePtr, W: WorkStructPtr>(workqueue: Q, work: W) -> bool {
    // SAFETY: Caller guarantees all preconditions of `try_queue_work`.
    unsafe { try_queue_work(workqueue, work) }
        .unwrap_or_else(|error| panic!("queue_work failed: {error:?}"))
}

/// Fallible implementation of [`queue_work`].
///
/// # Safety
/// `workqueue` must identify a live workqueue and `work` must point to a live,
/// initialized `WorkStruct`. Retain the work storage and callback state until
/// a later flush or synchronous cancellation observes it idle; do not modify
/// its list node or callback while it is queued or running.
pub unsafe fn try_queue_work<Q: WorkqueuePtr, W: WorkStructPtr>(
    workqueue: Q,
    work: W,
) -> Result<bool, WorkqueueError> {
    let workqueue = workqueue
        .workqueue_ptr()
        .cast_mut()
        .cast::<WorkqueueStruct>();
    let work = work.work_struct_ptr();
    if workqueue.is_null() || work.is_null() {
        return Err(WorkqueueError::NullPointer);
    }
    let _ = init_workqueue_executor();
    // SAFETY: `work` is non-null and points to a live `WorkStruct`.
    if unsafe { (*work).function }.is_none() {
        return Err(WorkqueueError::WorkNotInitialized);
    }

    {
        let mut queue = WORK_QUEUE.lock();
        // SAFETY: `work` points to a live `WorkStruct`.
        let state = unsafe { work_data(work) };
        let old = state.fetch_or(WORK_PENDING, Ordering::AcqRel);
        if old & WORK_PENDING != 0 {
            return Ok(false);
        }
        // SAFETY: `workqueue` and `work` are valid live pointers, and `queue` lock is held.
        unsafe { enqueue_locked(&mut queue, workqueue, work) };
    }
    notify_workers();
    kick_manager_if_starved();
    Ok(true)
}

/// Queue an already-claimed `irq_work` item for task-context execution by the
/// persistent shared worker pool. This path is allocation-free and never
/// starts or grows the pool, so it is safe for hard-IRQ producers.
///
/// The caller must have atomically claimed the item's PENDING and BUSY flags,
/// initialized its callback, and retain its storage until the callback clears
/// BUSY. A false return means the item was not enqueued; the caller is
/// responsible for clearing its claimed flags.
///
/// # Safety
/// `work` must point to a live `IrqWork` whose node is not queued or running,
/// and whose callback and storage remain valid until dispatch completes.
pub unsafe fn queue_irq_work(work: *mut IrqWork) -> bool {
    // SAFETY: `work` is non-null and points to a live `IrqWork`.
    if work.is_null() || unsafe { (*work).func }.is_none() {
        return false;
    }
    // SAFETY: `work` points to a live `IrqWork`. `node.flags.counter` is an aligned `u32` compatible with `AtomicU32`.
    let flags = unsafe {
        &*ptr::addr_of_mut!((*work).node.flags.counter).cast::<core::sync::atomic::AtomicU32>()
    };
    let claimed = crate::irq_work::IRQ_WORK_PENDING | crate::irq_work::IRQ_WORK_BUSY;
    if flags.load(Ordering::Acquire) & claimed != claimed {
        return false;
    }

    {
        let mut queue = WORK_QUEUE.lock();
        // SAFETY: `work` is live, claimed, and `queue` lock is held.
        unsafe { queue.push_irq(work) };
        IRQ_QUEUED.fetch_add(1, Ordering::AcqRel);
    }
    notify_workers();
    kick_manager_if_starved();
    true
}

/// Publish a work item whose PENDING bit is already set. The caller holds
/// `WORK_QUEUE`.
///
/// # Safety
/// `workqueue` and `work` must be live, properly aligned pointers. The caller must
/// hold `WORK_QUEUE` ensuring exclusive access to `queue`.
unsafe fn enqueue_locked(
    queue: &mut WorkList,
    workqueue: *mut WorkqueueStruct,
    work: *mut WorkStruct,
) {
    // SAFETY: Caller guarantees `workqueue` is a valid pointer.
    assert_eq!(
        unsafe { (*workqueue).magic },
        WORKQUEUE_MAGIC,
        "queue_work on a pointer that is not a workqueue_struct"
    );
    // SAFETY: Caller guarantees `work` points to a live `WorkStruct`.
    unsafe { work_data(work) }.store(workqueue as usize | WORK_PENDING, Ordering::Release);
    // SAFETY: Caller guarantees `workqueue` points to a live `WorkqueueStruct`.
    unsafe { (*workqueue).inflight.fetch_add(1, Ordering::AcqRel) };
    QUEUED.fetch_add(1, Ordering::AcqRel);
    // SAFETY: `work` is valid and `queue` lock is held.
    unsafe { queue.push(work) };
}

/// The workqueue recorded for a queued item, or null when the PENDING bit was
/// set without publishing the item (an RCU work item inside its grace period).
///
/// # Safety
/// `work` must point to a live, properly aligned `WorkStruct`.
unsafe fn queued_workqueue(work: *mut WorkStruct) -> *mut WorkqueueStruct {
    // SAFETY: Caller guarantees `work` is a valid pointer.
    let data = unsafe { work_data(work) }.load(Ordering::Acquire);
    if data & WORK_PENDING == 0 || data & !WORK_PENDING == WORK_DATA_INIT {
        return ptr::null_mut();
    }
    (data & !WORK_PENDING) as *mut WorkqueueStruct
}

/// Whether some worker is executing `work`. The caller holds `WORK_QUEUE`.
fn running_anywhere(work: *mut WorkStruct) -> bool {
    workers()
        .iter()
        .any(|slot| slot.current.load(Ordering::Acquire) == work)
}

/// Pop the oldest item that may run now: its queue is below `max_active` and
/// no other worker runs the same item. Skipped items keep their FIFO position,
/// so an ordered queue still runs in queue order. The caller holds
/// `WORK_QUEUE`.
///
/// # Safety
/// The caller must hold `WORK_QUEUE` ensuring exclusive access to `queue` and queued items.
unsafe fn take_runnable_locked(
    queue: &mut WorkList,
) -> Option<(*mut WorkStruct, *mut WorkqueueStruct)> {
    let head = queue.head;
    if head.is_null() {
        return None;
    }
    let mut node = head;
    loop {
        // SAFETY: `node` is a valid queued `ListHead` embedded in `WorkStruct`.
        let work = unsafe { work_from_entry(node) };
        // SAFETY: `work` is a valid queued `WorkStruct`.
        let workqueue = unsafe { queued_workqueue(work) };
        assert!(!workqueue.is_null(), "queued work item has no workqueue");
        // SAFETY: `workqueue` points to a live `WorkqueueStruct`.
        let limit = unsafe { (*workqueue).max_active };
        // SAFETY: `workqueue` points to a live `WorkqueueStruct`.
        if unsafe { (*workqueue).active.load(Ordering::Acquire) } < limit && !running_anywhere(work)
        {
            assert!(
                // SAFETY: `work` is a valid queued `WorkStruct` and queue lock is held.
                unsafe { queue.remove(work) },
                "runnable work missing from queue"
            );
            return Some((work, workqueue));
        }
        // SAFETY: `node` points to a valid list node under `WORK_QUEUE`.
        node = unsafe { (*node).next };
        if node == head {
            return None;
        }
    }
}

/// Whether any queued item could start now (manager heuristic).
fn has_runnable_work() -> bool {
    let queue = WORK_QUEUE.lock();
    if !queue.irq_head.is_null() {
        return true;
    }
    let head = queue.head;
    if head.is_null() {
        return false;
    }
    let mut node = head;
    loop {
        // SAFETY: `node` is a valid queued `ListHead` embedded in `WorkStruct`.
        let work = unsafe { work_from_entry(node) };
        // SAFETY: `work` is a valid queued `WorkStruct`.
        let workqueue = unsafe { queued_workqueue(work) };
        if !workqueue.is_null()
            // SAFETY: `workqueue` points to a live `WorkqueueStruct`.
            && unsafe { (*workqueue).active.load(Ordering::Acquire) < (*workqueue).max_active }
            && !running_anywhere(work)
        {
            return true;
        }
        // SAFETY: `node` points to a valid list node under `WORK_QUEUE`.
        node = unsafe { (*node).next };
        if node == head {
            return false;
        }
    }
}

/// Move one runnable item from the FIFO onto worker `index`.
fn take_work(index: usize) -> *mut WorkStruct {
    let taken = {
        let mut queue = WORK_QUEUE.lock();
        // SAFETY: `queue` lock is held, satisfying `take_runnable_locked` preconditions.
        let Some((work, workqueue)) = (unsafe { take_runnable_locked(&mut queue) }) else {
            return ptr::null_mut();
        };
        // The pending-to-running transition is made under the same lock used
        // by queue/cancel, so no producer can observe a detached-but-pending
        // item, and the item can be requeued as soon as it runs.
        // SAFETY: `work` is a valid `WorkStruct` dequeued under the queue lock.
        unsafe { work_data(work) }.store(WORK_DATA_INIT, Ordering::Release);
        QUEUED.fetch_sub(1, Ordering::AcqRel);
        // SAFETY: `workqueue` points to a live `WorkqueueStruct`.
        unsafe { (*workqueue).active.fetch_add(1, Ordering::AcqRel) };
        let slot = &WORKERS[index];
        slot.current_wq.store(workqueue, Ordering::Release);
        slot.current.store(work, Ordering::Release);
        work
    };
    kick_manager_if_starved();
    taken
}

/// Pop one IRQ callback from the shared FIFO. IRQ work does not consume a
/// workqueue's `max_active` budget, but the pool worker remains occupied until
/// its callback returns.
fn take_irq_work() -> *mut IrqWork {
    let work = {
        let mut queue = WORK_QUEUE.lock();
        // SAFETY: `queue` lock is held, satisfying `pop_irq` preconditions.
        let work = unsafe { queue.pop_irq() };
        if !work.is_null() {
            IRQ_QUEUED.fetch_sub(1, Ordering::AcqRel);
        }
        work
    };
    if !work.is_null() {
        kick_manager_if_starved();
    }
    work
}

fn finish_work(index: usize, work: *mut WorkStruct) {
    {
        let _queue = WORK_QUEUE.lock();
        let slot = &WORKERS[index];
        let active = slot.current.swap(ptr::null_mut(), Ordering::AcqRel);
        assert_eq!(active, work, "work callback completed without ownership");
        let workqueue = slot.current_wq.swap(ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: `workqueue` points to a live `WorkqueueStruct` previously associated with the active work.
        unsafe {
            (*workqueue).active.fetch_sub(1, Ordering::AcqRel);
            (*workqueue).inflight.fetch_sub(1, Ordering::AcqRel);
        }
    }
    crate::wait::wake_all();
    // A requeued instance of this item, or the next item of a saturated
    // queue, may be runnable now.
    if QUEUED.load(Ordering::Acquire) != 0 {
        notify_workers();
    }
}

fn work_busy(work: *mut WorkStruct) -> bool {
    let _queue = WORK_QUEUE.lock();
    // SAFETY: `work` is a valid `WorkStruct` pointer and queue lock protects reads.
    unsafe { work_data(work) }.load(Ordering::Acquire) & WORK_PENDING != 0 || running_anywhere(work)
}

fn workqueue_worker(index: usize) {
    let task = kernel_services::current_task_id()
        .expect("workqueue worker has no stable scheduler task identity");
    WORKERS[index].task.store(task as usize, Ordering::Release);
    let ready = || WORKER_STATE.load(Ordering::Acquire) == WORKER_READY;
    assert!(
        crate::wait::wait_until_ready(None, &ready),
        "workqueue worker could not wait for executor publication"
    );

    let mut irq_burst = 0;
    loop {
        let generation = WORK_GENERATION.load(Ordering::Acquire);

        if irq_burst < MAX_IRQ_WORK_BURST {
            let irq_work = take_irq_work();
            if !irq_work.is_null() {
                irq_burst += 1;
                // SAFETY: `irq_work` is a valid claimed IRQ work item popped from the queue.
                unsafe { crate::irq_work::run_irq_work(irq_work) };
                continue;
            }
        }

        let work = take_work(index);
        if !work.is_null() {
            irq_burst = 0;
            // SAFETY: `work` is a valid `WorkStruct` popped from the queue.
            let callback =
                unsafe { (*work).function }.expect("queued work item has no initialized callback");
            // SAFETY: `callback` is a valid C-ABI work function and `work` points to the active `WorkStruct`.
            unsafe { callback(work) };
            finish_work(index, work);
            continue;
        }

        // If no ordinary work could run, continue draining IRQ work even
        // after the fairness burst limit.
        let irq_work = take_irq_work();
        if !irq_work.is_null() {
            irq_burst = 1;
            // SAFETY: `irq_work` is a valid claimed IRQ work item popped from the queue.
            unsafe { crate::irq_work::run_irq_work(irq_work) };
            continue;
        }

        IDLE_WORKERS.fetch_add(1, Ordering::AcqRel);
        let changed = || WORK_GENERATION.load(Ordering::Acquire) != generation;
        assert!(
            crate::wait::wait_until_ready(None, &changed),
            "workqueue worker lost its waitable task context"
        );
        IDLE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Grow the pool when runnable work has waited `STARVATION_GRACE_NS` with no
/// idle worker: every worker is busy or blocked, possibly on that very work.
fn workqueue_manager() {
    assert!(
        crate::task_runtime::can_block_current(),
        "workqueue manager has no blockable task context"
    );
    let ready = || WORKER_STATE.load(Ordering::Acquire) == WORKER_READY;
    assert!(
        crate::wait::wait_until_ready(None, &ready),
        "workqueue manager could not wait for executor publication"
    );
    let starved = || {
        (QUEUED.load(Ordering::Acquire) != 0 || IRQ_QUEUED.load(Ordering::Acquire) != 0)
            && IDLE_WORKERS.load(Ordering::Acquire) == 0
            && NR_WORKERS.load(Ordering::Acquire) < MAX_WORKERS
    };
    loop {
        // Starvation is transient: a worker may take the queued item between
        // the wait provider's predicate check and this final recheck.
        if !crate::wait::wait_until_ready(None, &starved) {
            continue;
        }
        let never_ready = || false;
        let _ = crate::wait::wait_timeout_until(Some(STARVATION_GRACE_NS), &never_ready);
        if !starved() || !has_runnable_work() {
            continue;
        }
        match spawn_worker() {
            Ok(()) => log::debug!(
                "workqueue: added worker {} for starved work",
                NR_WORKERS.load(Ordering::Acquire)
            ),
            Err(error) => {
                log::warn!("workqueue: cannot add worker: {error:?}");
                let never_ready = || false;
                let _ = crate::wait::wait_timeout_until(
                    Some(crate::clock::NANOS_PER_SECOND),
                    &never_ready,
                );
            }
        }
    }
}

/// Return the work item the calling worker task is executing, or null when
/// the caller is not a pool worker.
pub fn current_work() -> *mut WorkStruct {
    let Some(task) = kernel_services::current_task_id() else {
        return ptr::null_mut();
    };
    workers()
        .iter()
        .find(|slot| slot.task.load(Ordering::Acquire) == task as usize)
        .map_or(ptr::null_mut(), |slot| slot.current.load(Ordering::Acquire))
}

/// Non-synchronously cancel a queued work instance.
///
/// # Safety
/// `work` must be a live initialized work item. The caller must retain it
/// until any executing instance has completed.
pub unsafe fn cancel_work<W: WorkStructPtr>(work: W) -> bool {
    let work = work.work_struct_ptr();
    assert!(!work.is_null(), "cancel_work called with null work");

    let removed = {
        let mut queue = WORK_QUEUE.lock();
        // SAFETY: `work` points to a live `WorkStruct`.
        let workqueue = unsafe { queued_workqueue(work) };
        if workqueue.is_null() {
            // Idle, or an RCU work item still inside its grace period, which
            // Linux also cannot steal.
            false
        } else {
            // SAFETY: queue lock serializes cancellation with enqueue/dequeue.
            assert!(
                unsafe { queue.remove(work) },
                "pending work missing from queue"
            );
            // SAFETY: `work` points to a live `WorkStruct`.
            unsafe { work_data(work) }.store(WORK_DATA_INIT, Ordering::Release);
            QUEUED.fetch_sub(1, Ordering::AcqRel);
            // SAFETY: `workqueue` points to a live `WorkqueueStruct`.
            unsafe { (*workqueue).inflight.fetch_sub(1, Ordering::AcqRel) };
            true
        }
    };
    if removed {
        crate::wait::wake_all();
    }
    removed
}

/// Wait for the queued and executing instances of `work` to finish.
///
/// # Safety
/// `work` must remain live and initialized through the wait. The caller must
/// be in task context and must not wait for itself from its own callback.
pub unsafe fn flush_work<W: WorkStructPtr>(work: W) -> bool {
    // SAFETY: Caller guarantees all preconditions of `try_flush_work`.
    unsafe { try_flush_work(work) }.unwrap_or_else(|error| panic!("flush_work failed: {error:?}"))
}

/// Fallible form of [`flush_work`].
///
/// # Safety
/// `work` must point to a live initialized `WorkStruct` and remain allocated
/// until it becomes idle. The caller must be in blockable task context and
/// must not wait for this work from its own callback.
pub unsafe fn try_flush_work<W: WorkStructPtr>(work: W) -> Result<bool, WorkqueueError> {
    let work = work.work_struct_ptr();
    if work.is_null() {
        return Err(WorkqueueError::NullPointer);
    }
    if !crate::task_runtime::can_block_current() {
        return Err(WorkqueueError::ContextCannotBlock);
    }
    let _ = init_workqueue_executor();
    if !work_busy(work) {
        return Ok(false);
    }
    let mut waited_s = 0u64;
    loop {
        let finished = || !work_busy(work);
        let timed_out =
            crate::wait::wait_timeout_until(Some(10 * crate::clock::NANOS_PER_SECOND), &finished);
        if !timed_out || !work_busy(work) {
            break;
        }
        waited_s += 10;
        report_flush_stall(work, waited_s);
    }
    Ok(true)
}

/// Name the waited item and what every worker is running.
fn report_flush_stall(work: *mut WorkStruct, waited_s: u64) {
    // SAFETY: `work` points to a live `WorkStruct`.
    let waited_fn = unsafe { (*work).function }.map_or(0, |f| f as usize);
    log::warn!(
        "flush_work stalled {waited_s}s: waiting work={work:p} fn={waited_fn:#x} data={:#x} \
         queued={} idle={}/{}",
        // SAFETY: `work` points to a live `WorkStruct`.
        unsafe { work_data(work) }.load(Ordering::Acquire),
        QUEUED.load(Ordering::Acquire),
        IDLE_WORKERS.load(Ordering::Acquire),
        NR_WORKERS.load(Ordering::Acquire),
    );
    for (index, slot) in workers().iter().enumerate() {
        let active = slot.current.load(Ordering::Acquire);
        if !active.is_null() {
            // SAFETY: `active` is a live `WorkStruct` currently executing on a worker slot.
            let active_fn = unsafe { (*active).function }.map_or(0, |f| f as usize);
            log::warn!("  kworker/{index} runs work={active:p} fn={active_fn:#x}");
        }
    }
}

/// Cancel pending work and wait for an already executing callback to finish.
///
/// # Safety
/// `work` must remain live and initialized through the cancellation. The
/// caller must be in task context and must not cancel itself synchronously.
pub unsafe fn cancel_work_sync<W: WorkStructPtr>(work: W) -> bool {
    let work = work.work_struct_ptr();
    assert!(!work.is_null(), "cancel_work_sync called with null work");
    if !crate::task_runtime::can_block_current() {
        panic!("cancel_work_sync requires blockable task context");
    }
    let _ = init_workqueue_executor();

    // SAFETY: Caller guarantees `work` satisfies `cancel_work` preconditions.
    let cancelled = unsafe { cancel_work(work) };
    let was_active = work_busy(work);
    if was_active {
        let finished = || !work_busy(work);
        assert!(
            crate::wait::wait_until_ready(None, &finished),
            "cancel_work_sync could not wait for the worker"
        );
    }
    cancelled || was_active
}

/// Set the PENDING bit for a caller that queues the item later (RCU-deferred
/// queueing). Returns false when the item was already pending.
///
/// # Safety
/// `work` must be a live, initialized work item.
pub unsafe fn mark_work_pending(work: *mut WorkStruct) -> bool {
    // SAFETY: Caller guarantees `work` points to a live `WorkStruct`.
    let old = unsafe { work_data(work) }.fetch_or(WORK_PENDING, Ordering::AcqRel);
    old & WORK_PENDING == 0
}

/// Enqueue on `workqueue` a work item whose PENDING bit the caller already
/// set. The pending bit stays set until a worker dequeues the item.
///
/// # Safety
/// `work` must be live, initialized, marked pending and not on the queue;
/// `workqueue` must be a live workqueue handle.
pub unsafe fn enqueue_marked_work(workqueue: *mut c_void, work: *mut WorkStruct) {
    assert!(!workqueue.is_null() && !work.is_null());
    let _ = init_workqueue_executor();
    {
        let mut queue = WORK_QUEUE.lock();
        // SAFETY: Caller guarantees `workqueue` and `work` are valid live pointers, and queue lock is held.
        unsafe { enqueue_locked(&mut queue, workqueue.cast(), work) };
    }
    notify_workers();
    kick_manager_if_starved();
}

/// Block until `workqueue` has no queued or running item.
///
/// This covers every item queued before the call, which is the Linux
/// guarantee of `flush_workqueue()` / `drain_workqueue()`; it may additionally
/// wait for items queued on the same queue after the call.
fn flush_workqueue_items(workqueue: *mut WorkqueueStruct) {
    assert!(!workqueue.is_null(), "flush_workqueue(NULL)");
    // SAFETY: `workqueue` points to a live `WorkqueueStruct`.
    if unsafe { (*workqueue).inflight.load(Ordering::Acquire) } == 0 {
        return;
    }
    if !crate::task_runtime::can_block_current() {
        panic!("flush_workqueue requires blockable task context");
    }
    let _ = init_workqueue_executor();
    // SAFETY: `workqueue` points to a live `WorkqueueStruct`.
    let drained = || unsafe { (*workqueue).inflight.load(Ordering::Acquire) } == 0;
    assert!(
        crate::wait::wait_until_ready(None, &drained),
        "flush_workqueue could not wait for the workers"
    );
}

// ---------------------------------------------------------------------------
// C ABI entry points.
// ---------------------------------------------------------------------------

/// Linux `queue_work()`. Returns false when the item is already pending.
///
/// # Safety
/// `wq` must point to a live workqueue and `work` to a live initialized item.
/// Retain the work storage and callback state until it is flushed or
/// synchronously cancelled; do not modify its list node or callback while it
/// is queued or running.
#[unsafe(export_name = "queue_work")]
pub unsafe extern "C" fn c_queue_work(wq: *mut c_void, work: *mut WorkStruct) -> bool {
    // SAFETY: Caller guarantees `wq` and `work` satisfy `queue_work` preconditions.
    unsafe { queue_work(wq, work) }
}

/// Linux `flush_work()`.
///
/// # Safety
/// `work` must point to a live initialized item and remain allocated through
/// the call. The caller must be in blockable task context and must not wait
/// for its own callback.
#[unsafe(export_name = "flush_work")]
pub unsafe extern "C" fn c_flush_work(work: *mut WorkStruct) -> bool {
    // SAFETY: Caller guarantees `work` satisfies `flush_work` preconditions.
    unsafe { flush_work(work) }
}

/// Linux `flush_workqueue()`.
///
/// # Safety
/// `wq` must identify a live workqueue handle. Keep it alive and prevent
/// concurrent destruction until this blocking drain returns. The caller must
/// be in blockable task context and must not wait from one of that queue's
/// callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn flush_workqueue(wq: *mut c_void) {
    flush_workqueue_items(wq.cast());
}

/// Linux `drain_workqueue()`: flushes until no work remains. Re-queueing
/// after the drain is not rejected, which is the one difference from Linux's
/// `__WQ_DRAINING` warning path.
///
/// # Safety
/// `wq` must identify a live workqueue handle. Keep it alive and prevent
/// concurrent destruction until this blocking drain returns. The caller must
/// be in blockable task context and must not wait from one of that queue's
/// callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn drain_workqueue(wq: *mut c_void) {
    flush_workqueue_items(wq.cast());
}

/// Linux `alloc_workqueue()`. The format arguments name the queue for debug
/// output and are not kept; `flags` and `max_active` set the concurrency
/// limit as in `__alloc_workqueue()` / `wq_clamp_max_active()`.
///
/// # Safety
/// The fixed and variadic arguments must follow the declared C ABI. This
/// adapter ignores `_fmt` and the variadic values; the returned handle is
/// owned by the caller and must be destroyed exactly once after all users and
/// queue operations have finished.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn alloc_workqueue(
    _fmt: *const core::ffi::c_char,
    flags: u32,
    max_active: core::ffi::c_int,
    _args: ...
) -> *mut c_void {
    let max_active = if flags & __WQ_ORDERED != 0 {
        1
    } else if max_active <= 0 {
        WQ_DFL_ACTIVE
    } else {
        (max_active as u32).min(WQ_MAX_ACTIVE)
    };
    let queue = alloc::boxed::Box::new(WorkqueueStruct::new(max_active));
    alloc::boxed::Box::into_raw(queue).cast()
}

/// Linux `destroy_workqueue()`: drain queued work, then release the handle.
/// The shared system queues are never destroyed.
///
/// # Safety
/// `wq` must be a unique live handle returned by `alloc_workqueue`, not a
/// shared system queue. No other thread may queue work on it or destroy it
/// concurrently. The caller must be in blockable task context and must not
/// destroy the queue from one of its own callbacks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn destroy_workqueue(wq: *mut c_void) {
    assert!(!wq.is_null(), "destroy_workqueue(NULL)");
    assert!(
        wq.cast::<WorkqueueStruct>() != system_dfl_wq
            && wq.cast::<WorkqueueStruct>() != system_highpri_wq,
        "destroy_workqueue on a system workqueue"
    );
    flush_workqueue_items(wq.cast());
    // SAFETY: Caller guarantees `wq` was allocated via `Box::into_raw` in `alloc_workqueue` and is no longer accessed.
    drop(unsafe { alloc::boxed::Box::from_raw(wq.cast::<WorkqueueStruct>()) });
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicU32;

    use super::*;
    use crate::task_runtime::test_serial_runtime;

    static TEST_CALLBACK_CALLED: AtomicU32 = AtomicU32::new(0);
    static IRQ_CALLBACK_CALLED: AtomicU32 = AtomicU32::new(0);
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        test_serial_runtime()
    }

    /// The host fixture rejects task creation; the tests drive worker slots by hand.
    fn claim_test_slots() {
        NR_WORKERS.fetch_max(2, Ordering::AcqRel);
    }

    fn new_work() -> alloc::boxed::Box<WorkStruct> {
        // SAFETY: Zeroing `WorkStruct` produces a valid initial bit pattern before `INIT_WORK_C`.
        let mut work = alloc::boxed::Box::new(unsafe { core::mem::zeroed::<WorkStruct>() });
        INIT_WORK_C(&mut work, test_callback);
        work
    }

    /// # Safety
    /// Valid C-ABI callback for unit tests.
    unsafe extern "C" fn test_callback(_work: *mut WorkStruct) {
        TEST_CALLBACK_CALLED.fetch_add(1, Ordering::SeqCst);
    }

    /// # Safety
    /// Valid C-ABI callback for unit tests.
    unsafe extern "C" fn test_irq_callback(_work: *mut IrqWork) {
        IRQ_CALLBACK_CALLED.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn irq_work_is_queued_without_inline_execution_and_retired_by_worker() {
        let _serial = serial();
        IRQ_CALLBACK_CALLED.store(0, Ordering::SeqCst);
        // SAFETY: Zeroing `IrqWork` produces a valid initial bit pattern before `init_irq_work`.
        let mut work = alloc::boxed::Box::new(unsafe { core::mem::zeroed::<IrqWork>() });
        crate::irq_work::init_irq_work(&mut *work, test_irq_callback);

        assert!(crate::irq_work::irq_work_queue(&mut *work));
        assert_eq!(IRQ_CALLBACK_CALLED.load(Ordering::SeqCst), 0);
        assert_eq!(IRQ_QUEUED.load(Ordering::Acquire), 1);
        assert!(!crate::irq_work::irq_work_queue(&mut *work));
        assert_eq!(IRQ_QUEUED.load(Ordering::Acquire), 1);

        let queued = take_irq_work();
        assert_eq!(queued, &mut *work as *mut IrqWork);
        assert_eq!(IRQ_QUEUED.load(Ordering::Acquire), 0);
        assert_eq!(IRQ_CALLBACK_CALLED.load(Ordering::SeqCst), 0);
        // SAFETY: `queued` is a valid `IrqWork` pointer from `take_irq_work`.
        unsafe { crate::irq_work::run_irq_work(queued) };
        assert_eq!(IRQ_CALLBACK_CALLED.load(Ordering::SeqCst), 1);
        // SAFETY: `queued` points to the live test `IrqWork`.
        let flags = unsafe {
            &*ptr::addr_of_mut!((*queued).node.flags.counter)
                .cast::<core::sync::atomic::AtomicU32>()
        };
        assert_eq!(flags.load(Ordering::Acquire), 0);
    }

    #[test]
    fn test_early_queue_work_and_cancel() {
        let _serial = serial();
        let mut work = core::mem::MaybeUninit::<WorkStruct>::zeroed();
        let work_ptr = work.as_mut_ptr();
        // SAFETY: `work_ptr` points to valid storage on stack initialized by `INIT_WORK_C`.
        unsafe {
            INIT_WORK_C(&mut *work_ptr, test_callback);
            assert!(!work_pending(work_ptr));
            // Queuing succeeds even when worker is unstarted / cannot block
            assert_eq!(try_queue_work(system_dfl_wq, work_ptr), Ok(true));
            assert!(work_pending(work_ptr));
            // Re-queuing returns Ok(false) because it's already pending
            assert_eq!(try_queue_work(system_dfl_wq, work_ptr), Ok(false));
            // Cancellation successfully removes it from queue and clears pending
            assert!(cancel_work(work_ptr));
            assert!(!work_pending(work_ptr));
            assert!(!cancel_work(work_ptr));
        }
    }

    #[test]
    fn test_cancel_work_before_execution() {
        let _serial = serial();
        let mut work = core::mem::MaybeUninit::<WorkStruct>::zeroed();
        let work_ptr = work.as_mut_ptr();
        // SAFETY: `work_ptr` points to valid storage on stack initialized by `INIT_WORK_C`.
        unsafe {
            INIT_WORK_C(&mut *work_ptr, test_callback);
            assert!(try_queue_work(system_dfl_wq, work_ptr).is_ok());
            let _ = cancel_work(work_ptr);
            assert!(!work_pending(work_ptr));
        }
    }

    #[test]
    fn ordered_queue_runs_one_item_at_a_time_in_order() {
        let _serial = serial();
        claim_test_slots();
        // SAFETY: `alloc_workqueue` called with valid C string and arguments.
        let wq = unsafe { alloc_workqueue(c"ordered".as_ptr(), __WQ_ORDERED, 1) };
        let mut first = new_work();
        let mut second = new_work();
        let (first, second) = (
            &mut *first as *mut WorkStruct,
            &mut *second as *mut WorkStruct,
        );
        // SAFETY: `wq`, `first`, and `second` are valid live pointers.
        unsafe {
            assert!(queue_work(wq, first));
            assert!(queue_work(wq, second));
        }
        assert_eq!(take_work(0), first);
        assert!(
            take_work(1).is_null(),
            "ordered queue started a second item"
        );
        finish_work(0, first);
        assert_eq!(take_work(1), second);
        finish_work(1, second);
        assert_eq!(
            // SAFETY: `wq` points to a live `WorkqueueStruct`.
            unsafe {
                (*wq.cast::<WorkqueueStruct>())
                    .inflight
                    .load(Ordering::Acquire)
            },
            0
        );
        // SAFETY: `wq` was returned by `alloc_workqueue` and is now idle.
        unsafe { destroy_workqueue(wq) };
    }

    #[test]
    fn unordered_queue_runs_items_concurrently() {
        let _serial = serial();
        claim_test_slots();
        let mut first = new_work();
        let mut second = new_work();
        let (first, second) = (
            &mut *first as *mut WorkStruct,
            &mut *second as *mut WorkStruct,
        );
        // SAFETY: `first` and `second` are live valid `WorkStruct`s.
        unsafe {
            assert!(queue_work(system_dfl_wq, first));
            assert!(queue_work(system_dfl_wq, second));
        }
        assert_eq!(take_work(0), first);
        assert_eq!(take_work(1), second);
        finish_work(0, first);
        finish_work(1, second);
    }

    #[test]
    fn requeued_running_item_waits_for_its_running_instance() {
        let _serial = serial();
        claim_test_slots();
        let mut work = new_work();
        let work = &mut *work as *mut WorkStruct;
        // SAFETY: `work` is a live valid `WorkStruct`.
        unsafe { assert!(queue_work(system_dfl_wq, work)) };
        assert_eq!(take_work(0), work);
        // SAFETY: `work` is a live valid `WorkStruct`.
        assert!(!unsafe { work_pending(work) });
        // SAFETY: `work` is a live valid `WorkStruct`.
        unsafe { assert!(queue_work(system_dfl_wq, work)) };
        assert!(
            take_work(1).is_null(),
            "work item ran on two workers at once"
        );
        finish_work(0, work);
        assert_eq!(take_work(1), work);
        finish_work(1, work);
        assert!(!work_busy(work));
    }
}
