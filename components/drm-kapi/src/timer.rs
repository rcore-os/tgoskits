// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Monotonic Linux timer-list adapter. A persistent kernel task waits until
//! the earliest jiffies deadline, then runs the timer callback asynchronously
//! with preemption disabled. This is task-context timer dispatch, not a native
//! softirq/timer-wheel implementation; callbacks are never run inline from
//! `mod_timer()`.

use alloc::string::String;
use core::{
    ffi::c_ulong,
    fmt::Write as _,
    hint::spin_loop,
    ops::{Deref, DerefMut},
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering},
};

use crate::{
    clock::{self, NANOS_PER_SECOND},
    fields::KtimeT,
    locks::{NoPreempt, NoPreemptIrqSave},
    types::{HlistNode, Hrtimer, TimerList},
};

pub const HRTIMER_NORESTART: i32 = 0;
pub const HRTIMER_RESTART: i32 = 1;
pub const HRTIMER_MODE_REL: i32 = 1;
pub const CLOCK_MONOTONIC: i32 = 1;
pub const NSEC_PER_USEC: u64 = 1_000;
pub const NSEC_PER_MSEC: u64 = 1_000_000;

/// Failure to initialize the asynchronous timer-list executor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerExecutorError {
    /// The caller cannot block, but executor startup or startup synchronization needs task context.
    ContextCannotBlock,
    /// The caller could not wait for another task to finish executor startup.
    WaitFailed,
    /// Allocating the task name failed.
    TaskNameAllocationFailed,
    /// The native task provider could not publish the timer worker.
    WorkerUnavailable,
}

/// Linux `ns_to_ktime()` for the target's scalar `ktime_t` representation.
#[inline]
pub const fn ns_to_ktime(ns: u64) -> KtimeT {
    ns as KtimeT
}

const TIMER_WORKER_UNINITIALIZED: u8 = 0;
const TIMER_WORKER_STARTING: u8 = 1;
const TIMER_WORKER_READY: u8 = 2;

const _: [(); core::mem::size_of::<usize>()] = [(); core::mem::size_of::<c_ulong>()];

/// Pointer/reference forms used by the source-order timer APIs.
pub trait TimerListPtr {
    fn timer_list_ptr(self) -> *mut TimerList;
}
impl TimerListPtr for *mut TimerList {
    fn timer_list_ptr(self) -> *mut TimerList {
        self
    }
}
impl TimerListPtr for *const TimerList {
    fn timer_list_ptr(self) -> *mut TimerList {
        self.cast_mut()
    }
}
impl TimerListPtr for &mut TimerList {
    fn timer_list_ptr(self) -> *mut TimerList {
        self
    }
}
impl TimerListPtr for &TimerList {
    fn timer_list_ptr(self) -> *mut TimerList {
        self as *const TimerList as *mut TimerList
    }
}

struct TimerListQueue {
    head: *mut HlistNode,
}
impl TimerListQueue {
    const fn new() -> Self {
        Self {
            head: ptr::null_mut(),
        }
    }

    /// Insert an unqueued timer. Caller owns TIMER_QUEUE.
    ///
    /// # Safety
    /// `timer` must point to a valid, properly aligned `TimerList` whose `entry`
    /// is currently unqueued (`pprev` is null). The caller must hold the queue lock
    /// ensuring exclusive access to both `self` and `timer->entry`.
    unsafe fn insert(&mut self, timer: *mut TimerList) {
        // SAFETY: Caller guarantees `timer` is non-null, properly aligned, and points to a live `TimerList`.
        let node = unsafe { ptr::addr_of_mut!((*timer).entry) };
        // SAFETY: `node` points to a valid `HlistNode` within the live `TimerList`.
        assert!(unsafe { (*node).pprev.is_null() });
        // SAFETY: `node` points to a valid `HlistNode` within `timer`. If `self.head` is non-null,
        // it points to a previously queued valid `HlistNode` whose fields can be updated under the queue lock.
        unsafe {
            (*node).next = self.head;
            (*node).pprev = ptr::addr_of_mut!(self.head);
            if !self.head.is_null() {
                (*self.head).pprev = ptr::addr_of_mut!((*node).next);
            }
        }
        self.head = node;
    }

    /// Remove a pending timer. Returns false when it is not queued.
    ///
    /// # Safety
    /// `timer` must point to a valid, properly aligned `TimerList`. The caller must
    /// hold the queue lock ensuring exclusive access to `self` and `timer->entry`.
    unsafe fn remove(&mut self, timer: *mut TimerList) -> bool {
        // SAFETY: Caller guarantees `timer` is non-null, aligned, and points to a valid `TimerList`.
        let node = unsafe { ptr::addr_of_mut!((*timer).entry) };
        // SAFETY: `node` points to the `entry` field of the valid `TimerList`.
        let previous = unsafe { (*node).pprev };
        if previous.is_null() {
            return false;
        }
        // SAFETY: `node` points to the `entry` field of the valid `TimerList`.
        let next = unsafe { (*node).next };
        // SAFETY: When `previous` is non-null, it points to either `self.head` or a predecessor node's `next` field,
        // both of which are valid under the queue lock. If `next` is non-null, it points to a valid successor node.
        unsafe {
            *previous = next;
            if !next.is_null() {
                (*next).pprev = previous;
            }
            (*node).next = ptr::null_mut();
            (*node).pprev = ptr::null_mut();
        }
        true
    }

    fn earliest(&self) -> *mut TimerList {
        let mut cursor = self.head;
        let mut earliest: *mut TimerList = ptr::null_mut();
        while !cursor.is_null() {
            // SAFETY: Every queued node is an embedded `TimerList.entry` inserted via `insert`.
            // Calculating `offset_of` yields the valid enclosing `TimerList` pointer.
            let timer = unsafe {
                cursor
                    .cast::<u8>()
                    .sub(core::mem::offset_of!(TimerList, entry))
                    .cast::<TimerList>()
            };
            // SAFETY: `timer` and `earliest` are non-null valid pointers to queued `TimerList` structs.
            if earliest.is_null()
                || time_before(unsafe { (*timer).expires }, unsafe { (*earliest).expires })
            {
                earliest = timer;
            }
            // SAFETY: `cursor` is non-null and points to a valid `HlistNode` in the intrusive list.
            cursor = unsafe { (*cursor).next };
        }
        earliest
    }
}
// SAFETY: `TimerListQueue` contains raw pointers forming an intrusive doubly linked list (`HlistNode`).
// It is `Send` because all operations and pointer modifications are guarded by external synchronization
// (such as `IrqSpinLock<TimerListQueue>`), and the queue owns its head pointer across thread transfers.
unsafe impl Send for TimerListQueue {}

struct IrqSpinLock<T> {
    held: AtomicBool,
    data: core::cell::UnsafeCell<T>,
}
// SAFETY: `IrqSpinLock` provides mutually exclusive access to `data` via an atomic spinlock combined
// with preemption and interrupt disabling (`NoPreemptIrqSave`). If `T: Send`, transferring ownership
// of `IrqSpinLock<T>` to another thread is safe because access across threads is serialized.
unsafe impl<T: Send> Send for IrqSpinLock<T> {}
// SAFETY: `IrqSpinLock` serializes all mutable access to `T` through `AtomicBool` and `IrqSpinGuard`.
// Multiple threads can hold references to `IrqSpinLock<T>` safely because only one thread can hold
// `IrqSpinGuard` at any given time.
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
    fn deref(&self) -> &T {
        // SAFETY: The spinlock `held` flag was acquired with Acquire ordering, ensuring exclusive
        // access to the inner `UnsafeCell` while this guard exists.
        unsafe { &*self.lock.data.get() }
    }
}
impl<T> DerefMut for IrqSpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: The spinlock `held` flag was acquired with Acquire ordering, and `&mut self`
        // guarantees unique exclusive access to the inner `UnsafeCell`.
        unsafe { &mut *self.lock.data.get() }
    }
}
impl<T> Drop for IrqSpinGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.held.store(false, Ordering::Release);
    }
}

static TIMER_QUEUE: IrqSpinLock<TimerListQueue> = IrqSpinLock::new(TimerListQueue::new());
static TIMER_ACTIVE: AtomicPtr<TimerList> = AtomicPtr::new(ptr::null_mut());
static TIMER_WORKER_STATE: AtomicU8 = AtomicU8::new(TIMER_WORKER_UNINITIALIZED);
static TIMER_GENERATION: AtomicUsize = AtomicUsize::new(0);

fn notify_timer_worker() {
    TIMER_GENERATION.fetch_add(1, Ordering::Release);
    crate::wait::wake_all();
}

fn time_before(left: u64, right: u64) -> bool {
    (left.wrapping_sub(right) as i64) < 0
}

fn timer_worker_name() -> Result<String, TimerExecutorError> {
    let mut name = String::new();
    name.try_reserve_exact(16)
        .map_err(|_| TimerExecutorError::TaskNameAllocationFailed)?;
    name.push_str("drm-timer");
    name.write_fmt(format_args!("-{}", 0))
        .map_err(|_| TimerExecutorError::TaskNameAllocationFailed)?;
    Ok(name)
}

/// Start the monotonic timer-list worker in task context. Timer setup calls
/// this before a timer can become visible to interrupt context; once started,
/// it succeeds in any context.
pub fn init_timer_executor() -> Result<(), TimerExecutorError> {
    loop {
        match TIMER_WORKER_STATE.load(Ordering::Acquire) {
            // An established executor serves every later caller, including
            // timer arming under an IRQ-saved lock where blocking is illegal.
            TIMER_WORKER_READY => return Ok(()),
            TIMER_WORKER_STARTING => {
                if !crate::task_runtime::can_block_current() {
                    return Err(TimerExecutorError::ContextCannotBlock);
                }
                let ready = || TIMER_WORKER_STATE.load(Ordering::Acquire) != TIMER_WORKER_STARTING;
                if !crate::wait::wait_until_ready(None, &ready) {
                    return Err(TimerExecutorError::WaitFailed);
                }
            }
            TIMER_WORKER_UNINITIALIZED => {
                // Only the first start needs a blockable task context.
                if !crate::task_runtime::can_block_current() {
                    return Err(TimerExecutorError::ContextCannotBlock);
                }
                if TIMER_WORKER_STATE
                    .compare_exchange(
                        TIMER_WORKER_UNINITIALIZED,
                        TIMER_WORKER_STARTING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_err()
                {
                    continue;
                }
                let name = match timer_worker_name() {
                    Ok(name) => name,
                    Err(error) => {
                        TIMER_WORKER_STATE.store(TIMER_WORKER_UNINITIALIZED, Ordering::Release);
                        crate::wait::wake_all();
                        return Err(error);
                    }
                };
                if crate::task_runtime::spawn(name, alloc::boxed::Box::new(timer_worker)).is_err() {
                    TIMER_WORKER_STATE.store(TIMER_WORKER_UNINITIALIZED, Ordering::Release);
                    crate::wait::wake_all();
                    return Err(TimerExecutorError::WorkerUnavailable);
                }
                TIMER_WORKER_STATE.store(TIMER_WORKER_READY, Ordering::Release);
                crate::wait::wake_all();
                return Ok(());
            }
            _ => unreachable!("invalid timer worker state"),
        }
    }
}

/// Linux `timer_setup()` for the source `timer_list` layout.
///
/// # Safety
/// `timer` must point to writable, properly aligned `TimerList` storage that
/// remains allocated until synchronous deletion completes. It must not be
/// queued or have a callback running. `callback` must be a valid C-ABI
/// function, and any state it accesses must remain alive while the timer may
/// run. The caller must exclusively initialize the timer's list links and
/// callback fields.
pub unsafe fn timer_setup<T: TimerListPtr>(
    timer: T,
    callback: unsafe extern "C" fn(*mut TimerList),
    flags: u32,
) {
    init_timer_executor().expect("failed to bind Linux timer-list executor");
    let timer = timer.timer_list_ptr();
    assert!(!timer.is_null());
    let mut queue = TIMER_QUEUE.lock();
    // SAFETY: `timer` is non-null, aligned, and valid by caller preconditions.
    assert!(unsafe { (*timer).entry.pprev.is_null() });
    // SAFETY: `timer` points to a live `TimerList` and queue lock is held.
    let _ = unsafe { queue.remove(timer) };
    // SAFETY: Caller guarantees exclusive initialization of the timer's fields.
    unsafe {
        (*timer).entry.next = ptr::null_mut();
        (*timer).entry.pprev = ptr::null_mut();
        (*timer).expires = 0;
        (*timer).function = Some(callback);
        (*timer).flags = flags;
    }
}

/// Insert or update a timer at the absolute Linux jiffies deadline.
///
/// # Safety
/// `timer` must point to a live timer previously initialized by `timer_setup`.
/// Keep its storage and callback state alive until it is synchronously deleted;
/// callers must not modify the timer's list links or callback concurrently.
pub unsafe fn mod_timer<T: TimerListPtr>(timer: T, expires: c_ulong) -> bool {
    let timer = timer.timer_list_ptr();
    assert!(!timer.is_null());
    assert_eq!(
        TIMER_WORKER_STATE.load(Ordering::Acquire),
        TIMER_WORKER_READY,
        "timer scheduled before executor setup"
    );
    // SAFETY: `timer` is guaranteed non-null and pointing to an initialized `TimerList` by caller preconditions.
    assert!(
        unsafe { (*timer).function }.is_some(),
        "uninitialized Linux timer"
    );
    let mut queue = TIMER_QUEUE.lock();
    // SAFETY: `timer` is a valid `TimerList` pointer and queue lock is held.
    let was_pending = unsafe { queue.remove(timer) };
    // SAFETY: `timer` is valid and queue lock protects concurrent access.
    unsafe {
        (*timer).expires = expires;
    }
    // SAFETY: `timer` is unqueued and valid, and queue lock is held.
    unsafe {
        queue.insert(timer);
    }
    drop(queue);
    notify_timer_worker();
    was_pending
}

/// Schedule `timer` after a relative millisecond delay.
///
/// # Safety
/// `timer` must satisfy the same lifetime and initialization requirements as
/// [`mod_timer`], and its storage must remain live until synchronous deletion.
pub unsafe fn set_timer_ms<T: TimerListPtr>(timer: T, delay_ms: u64) -> bool {
    let ticks = delay_ms
        .saturating_mul(crate::config::CONFIG_HZ as u64)
        .saturating_add(999)
        / 1000;
    // SAFETY: Caller guarantees `timer` satisfies all `mod_timer` preconditions.
    unsafe {
        mod_timer(
            timer,
            crate::primitives::jiffies().wrapping_add(ticks) as c_ulong,
        )
    }
}

/// Remove a pending timer without waiting for a callback already running.
///
/// # Safety
/// `timer` must point to a live initialized timer. If a callback is running,
/// its storage and callback state must remain valid until that callback returns;
/// use [`del_timer_sync`] before releasing them.
pub unsafe fn del_timer<T: TimerListPtr>(timer: T) -> bool {
    let timer = timer.timer_list_ptr();
    assert!(!timer.is_null());
    // SAFETY: `timer` is non-null, valid, and queue lock is held.
    let removed = unsafe { TIMER_QUEUE.lock().remove(timer) };
    if removed {
        notify_timer_worker();
    }
    removed
}

/// Synchronously delete a timer, including a callback currently in progress.
///
/// # Safety
/// `timer` must point to a live initialized timer and remain allocated through
/// this call. The caller must be in blockable task context and must not invoke
/// this from the timer's own callback.
pub unsafe fn del_timer_sync<T: TimerListPtr>(timer: T) -> bool {
    let timer = timer.timer_list_ptr();
    assert!(!timer.is_null());
    if !crate::task_runtime::can_block_current() {
        panic!("del_timer_sync requires task context");
    }
    let mut was_pending = false;
    loop {
        let (removed, active) = {
            let mut queue = TIMER_QUEUE.lock();
            // SAFETY: `timer` is non-null, valid, and queue lock is held.
            let removed = unsafe { queue.remove(timer) };
            (removed, TIMER_ACTIVE.load(Ordering::Acquire) == timer)
        };
        was_pending |= removed;
        notify_timer_worker();
        if active {
            let callback_done = || TIMER_ACTIVE.load(Ordering::Acquire) != timer;
            if !crate::wait::wait_until_ready(None, &callback_done) {
                panic!("timer cancellation lost its task wait context");
            }
            continue;
        }
        break;
    }
    was_pending
}

/// Source-name used by the imported Execlists code for synchronous deletion.
///
/// # Safety
/// `timer` must satisfy [`del_timer_sync`] requirements: it must be a live,
/// initialized timer, and the caller must be in blockable task context outside
/// its callback.
pub unsafe fn cancel_timer<T: TimerListPtr>(timer: T) {
    // SAFETY: Caller guarantees `timer` satisfies all `del_timer_sync` preconditions.
    let _ = unsafe { del_timer_sync(timer) };
}

/// Return whether a live initialized timer is currently queued.
///
/// # Safety
/// `timer` must point to valid initialized `TimerList` storage that is not
/// concurrently freed or reinitialized.
pub unsafe fn timer_pending<T: TimerListPtr>(timer: T) -> bool {
    let timer = timer.timer_list_ptr();
    assert!(!timer.is_null());
    let _queue = TIMER_QUEUE.lock();
    // SAFETY: `timer` is non-null, valid, and queue lock protects reading `entry.pprev`.
    unsafe { !(*timer).entry.pprev.is_null() }
}

/// Whether the timer is queued or its callback is currently executing.
///
/// # Safety
/// `timer` must point to valid initialized `TimerList` storage that is not
/// concurrently freed or reinitialized.
pub unsafe fn timer_active<T: TimerListPtr>(timer: T) -> bool {
    let timer = timer.timer_list_ptr();
    assert!(!timer.is_null());
    let _queue = TIMER_QUEUE.lock();
    // SAFETY: `timer` is non-null, valid, and queue lock protects reading `entry.pprev`.
    (unsafe { !(*timer).entry.pprev.is_null() }) || TIMER_ACTIVE.load(Ordering::Acquire) == timer
}

/// Whether an active timer has reached its stored jiffies deadline.
///
/// # Safety
/// `timer` must point to valid initialized `TimerList` storage that is not
/// concurrently freed or reinitialized.
pub unsafe fn timer_expired<T: TimerListPtr>(timer: T) -> bool {
    let timer = timer.timer_list_ptr();
    assert!(!timer.is_null());
    let _queue = TIMER_QUEUE.lock();
    // SAFETY: `timer` is non-null, valid, and queue lock protects reading `entry.pprev`.
    let pending = unsafe { !(*timer).entry.pprev.is_null() };
    let active = TIMER_ACTIVE.load(Ordering::Acquire) == timer;
    // SAFETY: `timer` is non-null, valid, and queue lock protects reading `expires`.
    (pending || active) && !time_before(crate::primitives::jiffies(), unsafe { (*timer).expires })
}

/// Linux 7.2.3 source spelling for non-synchronous timer deletion.
///
/// # Safety
/// `timer` must satisfy [`del_timer`] requirements. A running callback may
/// continue after this call returns.
pub unsafe fn timer_delete<T: TimerListPtr>(timer: T) -> bool {
    // SAFETY: Caller guarantees `timer` satisfies all `del_timer` preconditions.
    unsafe { del_timer(timer) }
}

/// Linux 7.2.3 source spelling for synchronous timer deletion.
///
/// # Safety
/// `timer` must satisfy [`del_timer_sync`] requirements: it must be live and
/// initialized, and the caller must be in blockable task context outside its
/// callback.
pub unsafe fn timer_delete_sync<T: TimerListPtr>(timer: T) -> bool {
    // SAFETY: Caller guarantees `timer` satisfies all `del_timer_sync` preconditions.
    unsafe { del_timer_sync(timer) }
}

fn timer_worker() {
    let ready = || TIMER_WORKER_STATE.load(Ordering::Acquire) == TIMER_WORKER_READY;
    assert!(
        crate::wait::wait_until_ready(None, &ready),
        "timer worker could not wait for executor publication"
    );
    loop {
        let generation = TIMER_GENERATION.load(Ordering::Acquire);
        let (timer, expires) = {
            let mut queue = TIMER_QUEUE.lock();
            let timer = queue.earliest();
            if timer.is_null() {
                (timer, 0)
            } else {
                // SAFETY: `timer` is non-null, obtained from `queue.earliest()` while holding the queue lock.
                let expires = unsafe { (*timer).expires };
                if !time_before(crate::primitives::jiffies(), expires) {
                    // SAFETY: `timer` is a valid queued timer and the queue lock is held.
                    let _ = unsafe { queue.remove(timer) };
                    assert!(
                        TIMER_ACTIVE
                            .compare_exchange(
                                ptr::null_mut(),
                                timer,
                                Ordering::AcqRel,
                                Ordering::Acquire
                            )
                            .is_ok()
                    );
                }
                (timer, expires)
            }
        };
        if timer.is_null() {
            if TIMER_GENERATION.load(Ordering::Acquire) == generation {
                let changed = || TIMER_GENERATION.load(Ordering::Acquire) != generation;
                assert!(
                    crate::wait::wait_until_ready(None, &changed),
                    "timer worker lost its task wait context"
                );
            }
            continue;
        }
        if TIMER_ACTIVE.load(Ordering::Acquire) == timer {
            // SAFETY: `timer` was queued and is now active, so its callback function pointer is valid.
            let callback = unsafe { (*timer).function }.expect("timer has no callback");
            {
                let _atomic_timer_context = NoPreempt::new();
                // SAFETY: `timer` points to a live `TimerList` whose storage and callback state remain valid.
                unsafe { callback(timer) };
            }
            {
                let _queue = TIMER_QUEUE.lock();
                assert_eq!(TIMER_ACTIVE.load(Ordering::Acquire), timer);
                TIMER_ACTIVE.store(ptr::null_mut(), Ordering::Release);
            }
            crate::wait::wake_all();
            continue;
        }

        let now = crate::primitives::jiffies();
        if time_before(now, expires) {
            let remaining_ticks = expires.wrapping_sub(now);
            let hz = crate::config::CONFIG_HZ as u64;
            let target_tick = now as u128 + remaining_ticks as u128;
            let target_ns = (target_tick * NANOS_PER_SECOND as u128).div_ceil(hz as u128);
            let now_ns = clock::now_ns() as u128;
            let nanos = target_ns.saturating_sub(now_ns).min(u64::MAX as u128) as u64;
            let changed = || TIMER_GENERATION.load(Ordering::Acquire) != generation;
            let _ = crate::wait::wait_timeout_until(Some(nanos.max(1)), &changed);
        }
    }
}

// ---------------------------------------------------------------------------
// High-resolution timer API over the timer-list worker.
//
// Hrtimers retain nanosecond deadlines and use ClockOps' monotonic clock, but
// dispatch is rounded up to the next jiffy. With the current CONFIG_HZ=100,
// the scheduling resolution is 10 ms (plus task-scheduler latency), not Linux
// native hrtimer precision. Deadlines are never intentionally fired early.
// ---------------------------------------------------------------------------

const HRTIMER_MODE_ABS_VALUE: i32 = 0;

#[repr(C)]
struct HrtimerImpl {
    timer: TimerList,
    function: Option<unsafe extern "C" fn(*mut Hrtimer) -> i32>,
    expires_ns: i64,
    cancelled: bool,
    _reserved: [u8; 7],
}
const _: () = assert!(core::mem::size_of::<HrtimerImpl>() <= core::mem::size_of::<Hrtimer>());
const _: () = assert!(core::mem::align_of::<HrtimerImpl>() <= core::mem::align_of::<Hrtimer>());

fn monotonic_now_ns() -> i64 {
    clock::now_ns().min(i64::MAX as u64) as i64
}

/// Round an absolute nanosecond deadline up to the first covering jiffy.
fn hrtimer_target_jiffies(expires_ns: i64, now_ns: i64, now_jiffies: u64) -> c_ulong {
    let target = if expires_ns <= now_ns {
        now_jiffies
    } else {
        // `jiffies()` is the floor of absolute monotonic time in HZ units.
        // Round the absolute deadline up, rather than rounding its delta from
        // now: ignoring the current tick phase could fire slightly early.
        ((expires_ns as u128) * (crate::config::CONFIG_HZ as u128))
            .div_ceil(NANOS_PER_SECOND as u128)
            .min(u64::MAX as u128) as u64
    };
    target.max(now_jiffies) as c_ulong
}

/// Arm the timer-list entry while the caller owns `TIMER_QUEUE`.
///
/// # Safety
/// `h` must point to a live, properly aligned `HrtimerImpl`. The caller must
/// hold `TIMER_QUEUE`, guaranteeing exclusive access to `queue` and the timer's list node.
unsafe fn arm_hrtimer_locked(
    h: *mut HrtimerImpl,
    queue: &mut TimerListQueue,
    now_ns: i64,
    now_jiffies: u64,
) -> bool {
    let timer = h.cast::<TimerList>();
    // SAFETY: `timer` is the embedded `TimerList` in `h`, which is valid and the queue lock is held.
    let was_pending = unsafe { queue.remove(timer) };
    // SAFETY: `h` points to a valid `HrtimerImpl` and the queue lock is held.
    let target = hrtimer_target_jiffies(unsafe { (*h).expires_ns }, now_ns, now_jiffies);
    // SAFETY: `timer` points to the valid embedded `TimerList` and the queue lock is held.
    unsafe {
        (*timer).expires = target;
        queue.insert(timer);
    }
    was_pending
}

/// Arm the timer-list entry of `h` for its absolute deadline.
///
/// # Safety
/// `h` must point to a live, properly aligned `HrtimerImpl` whose storage remains valid.
unsafe fn arm_hrtimer(h: *mut HrtimerImpl) -> bool {
    let mut queue = TIMER_QUEUE.lock();
    // SAFETY: `h` points to a live `HrtimerImpl` and queue lock protects reads.
    let was_pending = if unsafe { (*h).cancelled } {
        false
    } else {
        // SAFETY: `h` is valid and queue lock is held, satisfying `arm_hrtimer_locked` requirements.
        unsafe {
            arm_hrtimer_locked(
                h,
                &mut queue,
                monotonic_now_ns(),
                crate::primitives::jiffies(),
            )
        }
    };
    drop(queue);
    notify_timer_worker();
    was_pending
}

/// Apply the callback's restart result using caller-supplied time, while the
/// caller owns the queue lock. This also makes the restart transition directly
/// testable without sleeping or a running timer worker.
///
/// # Safety
/// `h` must point to a live, properly aligned `HrtimerImpl`. The caller must
/// hold the queue lock guaranteeing exclusive access to `queue` and `h`.
unsafe fn rearm_hrtimer_after_callback(
    h: *mut HrtimerImpl,
    callback_result: i32,
    queue: &mut TimerListQueue,
    now_ns: i64,
    now_jiffies: u64,
) -> bool {
    // SAFETY: `h` is valid and queue lock is held.
    if callback_result != HRTIMER_RESTART || unsafe { (*h).cancelled } {
        return false;
    }
    // SAFETY: `h` is valid and queue lock is held, satisfying `arm_hrtimer_locked` requirements.
    let _ = unsafe { arm_hrtimer_locked(h, queue, now_ns, now_jiffies) };
    true
}

/// High-resolution timer callback executing from timer worker context.
///
/// # Safety
/// `timer` must point to a valid `TimerList` embedded as the first field of an `HrtimerImpl`.
unsafe extern "C" fn hrtimer_timer_fn(timer: *mut TimerList) {
    let h = timer.cast::<HrtimerImpl>();
    let now_ns = monotonic_now_ns();
    // SAFETY: `h` points to a valid `HrtimerImpl`, layout-compatible with `Hrtimer`.
    let expires_ns = unsafe { hrtimer_get_expires(h.cast::<Hrtimer>()) };
    if now_ns < expires_ns {
        // SAFETY: `h` is a valid pointer to `HrtimerImpl`.
        let _ = unsafe { arm_hrtimer(h) };
        return;
    }
    // SAFETY: `h` points to a live `HrtimerImpl`.
    if let Some(function) = unsafe { (*h).function } {
        // SAFETY: `function` is a valid C-ABI hrtimer callback and `h` points to a live `Hrtimer`.
        let result = unsafe { function(h.cast::<Hrtimer>()) };
        let mut queue = TIMER_QUEUE.lock();
        // SAFETY: `h` is a valid `HrtimerImpl` and queue lock is held.
        let rearmed = unsafe {
            rearm_hrtimer_after_callback(
                h,
                result,
                &mut queue,
                monotonic_now_ns(),
                crate::primitives::jiffies(),
            )
        };
        drop(queue);
        if rearmed {
            notify_timer_worker();
        }
    }
}

/// Linux `hrtimer_setup()`.
///
/// # Safety
/// `timer` must point to writable, properly aligned `Hrtimer` storage large
/// enough for the adapter's private representation, and it must not be queued
/// or executing. `function`, when present, and any state it accesses must stay
/// valid until the timer has been synchronously cancelled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hrtimer_setup(
    timer: *mut Hrtimer,
    function: Option<unsafe extern "C" fn(*mut Hrtimer) -> i32>,
    _clock_id: i32,
    _mode: i32,
) {
    let h = timer.cast::<HrtimerImpl>();
    // SAFETY: Caller guarantees `timer` points to writable, properly aligned `Hrtimer` storage
    // large enough for `HrtimerImpl`, and that exclusive setup is performed.
    unsafe {
        (*h).function = function;
        (*h).expires_ns = 0;
        (*h).cancelled = false;
        timer_setup(h.cast::<TimerList>(), hrtimer_timer_fn, 0);
    }
}

/// Linux `hrtimer_start_range_ns()`. `range_ns` is currently ignored and is
/// not used for timer coalescing. The absolute deadline is rounded up to the
/// next jiffy and callback scheduling can add further latency; no slack-window
/// precision guarantee is made.
///
/// # Safety
/// `timer` must be a live `Hrtimer` initialized by [`hrtimer_setup`], with its
/// callback state retained until synchronous cancellation completes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hrtimer_start_range_ns(
    timer: *mut Hrtimer,
    time: KtimeT,
    _range_ns: u64,
    mode: i32,
) {
    let h = timer.cast::<HrtimerImpl>();
    assert_eq!(
        TIMER_WORKER_STATE.load(Ordering::Acquire),
        TIMER_WORKER_READY,
        "hrtimer scheduled before executor setup"
    );
    // SAFETY: Caller guarantees `timer` points to an initialized `HrtimerImpl`.
    assert!(
        unsafe { (*h).timer.function }.is_some(),
        "uninitialized hrtimer"
    );
    let mut queue = TIMER_QUEUE.lock();
    let now_ns = monotonic_now_ns();
    // SAFETY: `h` points to a live `HrtimerImpl` and queue lock is held, satisfying `arm_hrtimer_locked`.
    unsafe {
        (*h).cancelled = false;
        (*h).expires_ns = if mode == HRTIMER_MODE_ABS_VALUE {
            time
        } else {
            now_ns.saturating_add(time)
        };
        let _ = arm_hrtimer_locked(h, &mut queue, now_ns, crate::primitives::jiffies());
    }
    drop(queue);
    notify_timer_worker();
}

/// Return whether the hrtimer is queued or its callback is currently running.
///
/// # Safety
/// `timer` must point to a live hrtimer initialized by [`hrtimer_setup`] and
/// must not be concurrently freed or reinitialized.
pub unsafe fn hrtimer_active(timer: *const Hrtimer) -> bool {
    assert!(!timer.is_null());
    let timer = timer.cast_mut().cast::<TimerList>();
    let _queue = TIMER_QUEUE.lock();
    // SAFETY: `timer` points to a valid `TimerList` within `Hrtimer`, and queue lock protects reads.
    (unsafe { !(*timer).entry.pprev.is_null() }) || TIMER_ACTIVE.load(Ordering::Acquire) == timer
}

/// Return the stored absolute monotonic deadline in nanoseconds.
///
/// # Safety
/// `timer` must point to a live hrtimer initialized by [`hrtimer_setup`] and
/// must not be concurrently freed or reinitialized.
pub unsafe fn hrtimer_get_expires(timer: *const Hrtimer) -> KtimeT {
    assert!(!timer.is_null());
    let h = timer.cast::<HrtimerImpl>();
    let _queue = TIMER_QUEUE.lock();
    // SAFETY: `h` points to a live `HrtimerImpl` and queue lock protects reads.
    unsafe { (*h).expires_ns }
}

fn forward_deadline(expires_ns: i64, now_ns: i64, interval_ns: KtimeT) -> (i64, u64) {
    if interval_ns <= 0 || now_ns < expires_ns {
        return (expires_ns, 0);
    }
    let interval = interval_ns as i128;
    let overruns = ((now_ns as i128 - expires_ns as i128) / interval) + 1;
    let next = expires_ns as i128 + overruns * interval;
    (
        next.min(i64::MAX as i128) as i64,
        overruns.min(u64::MAX as i128) as u64,
    )
}

/// Move the hrtimer's deadline to the first interval boundary after now.
/// Returns how many expirations were passed; a non-positive interval or a
/// deadline still in the future leaves the timer unchanged and returns zero.
///
/// # Safety
/// `timer` must point to a live hrtimer initialized by [`hrtimer_setup`] and
/// must not be concurrently freed or reinitialized.
pub unsafe fn hrtimer_forward_now(timer: *mut Hrtimer, interval: KtimeT) -> u64 {
    assert!(!timer.is_null());
    let h = timer.cast::<HrtimerImpl>();
    let _queue = TIMER_QUEUE.lock();
    // SAFETY: `h` points to a live `HrtimerImpl` and queue lock protects reads.
    let (next, overruns) =
        forward_deadline(unsafe { (*h).expires_ns }, monotonic_now_ns(), interval);
    if overruns != 0 {
        // SAFETY: `h` points to a live `HrtimerImpl` and queue lock protects writes.
        unsafe { (*h).expires_ns = next };
    }
    overruns
}

/// Linux `hrtimer_try_to_cancel()`: 1 when a pending timer was cancelled, 0
/// when it was not armed, -1 when its callback is running.
///
/// # Safety
/// `timer` must point to a live initialized `Hrtimer`. If this returns `-1`,
/// retain its storage and callback state until [`hrtimer_cancel`] completes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hrtimer_try_to_cancel(timer: *mut Hrtimer) -> i32 {
    assert!(!timer.is_null());
    let hrtimer = timer.cast::<HrtimerImpl>();
    let h = hrtimer.cast::<TimerList>();
    let mut queue = TIMER_QUEUE.lock();
    // SAFETY: `hrtimer` points to a live `HrtimerImpl` and queue lock protects writes.
    unsafe { (*hrtimer).cancelled = true };
    // SAFETY: `h` points to the valid embedded `TimerList` in `hrtimer` and queue lock is held.
    let removed = unsafe { queue.remove(h) };
    let active = TIMER_ACTIVE.load(Ordering::Acquire) == h;
    drop(queue);
    if removed {
        notify_timer_worker();
    }
    if active {
        -1
    } else if removed {
        1
    } else {
        0
    }
}

/// Linux `hrtimer_cancel()`: cancel and wait for a running callback.
///
/// # Safety
/// `timer` must point to a live initialized `Hrtimer`, and the caller must be
/// in blockable task context outside that timer's callback. Keep storage alive
/// through this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hrtimer_cancel(timer: *mut Hrtimer) -> i32 {
    assert!(!timer.is_null());
    {
        let _queue = TIMER_QUEUE.lock();
        // SAFETY: `timer` points to a live `HrtimerImpl` and queue lock protects writes.
        unsafe { (*timer.cast::<HrtimerImpl>()).cancelled = true };
    }
    // SAFETY: `timer` points to a live `Hrtimer` whose embedded `TimerList` is valid, and caller satisfies `del_timer_sync` preconditions.
    unsafe { del_timer_sync(timer.cast::<TimerList>()) as i32 }
}

/// Linux `timer_shutdown_sync()`: synchronous deletion. Returns whether the
/// timer was pending. Re-arming after shutdown is not rejected here.
///
/// # Safety
/// `timer` must point to a live initialized `TimerList`; the caller must be in
/// blockable task context and must not call this from the timer's callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn timer_shutdown_sync(timer: *mut TimerList) -> i32 {
    // SAFETY: Caller guarantees `timer` satisfies all `del_timer_sync` preconditions.
    unsafe { del_timer_sync(timer) as i32 }
}

/// Linux `round_jiffies_common()` for CPU 0 with `force_up` semantics: round
/// `j` up to the next whole second when it is still in the future.
fn round_jiffies_up_abs(j: u64) -> u64 {
    let hz = crate::config::CONFIG_HZ as u64;
    let now = crate::primitives::jiffies();
    let rem = j % hz;
    let rounded = j - rem + hz;
    if (now.wrapping_sub(rounded) as i64) < 0 {
        rounded
    } else {
        j
    }
}

/// Linux `round_jiffies_up()`.
#[unsafe(no_mangle)]
pub extern "C" fn round_jiffies_up(j: c_ulong) -> c_ulong {
    round_jiffies_up_abs(j) as c_ulong
}

/// Linux `round_jiffies_up_relative()`.
#[unsafe(no_mangle)]
pub extern "C" fn round_jiffies_up_relative(j: c_ulong) -> c_ulong {
    let now = crate::primitives::jiffies();
    round_jiffies_up_abs(now.wrapping_add(j)).wrapping_sub(now) as c_ulong
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hrtimer(expires_ns: i64) -> HrtimerImpl {
        HrtimerImpl {
            timer: TimerList {
                entry: HlistNode {
                    next: ptr::null_mut(),
                    pprev: ptr::null_mut(),
                },
                expires: 0,
                function: None,
                flags: 0,
            },
            function: None,
            expires_ns,
            cancelled: false,
            _reserved: [0; 7],
        }
    }

    #[test]
    fn hrtimer_forward_deadline_and_restart_rearm_are_consistent() {
        let (next_deadline, overruns) = forward_deadline(100_000_000, 265_000_000, 50_000_000);
        assert_eq!(overruns, 4);
        assert_eq!(next_deadline, 300_000_000);

        // A deadline is rounded from the absolute clock epoch, so a 20.5 ms
        // deadline observed at 10.5 ms waits for tick 3 (30 ms), not tick 2.
        assert_eq!(hrtimer_target_jiffies(20_500_000, 10_500_000, 1), 3);
        assert_eq!(hrtimer_target_jiffies(20_000_000, 10_500_000, 1), 2);

        let mut h = test_hrtimer(next_deadline);
        let mut queue = TimerListQueue::new();
        // SAFETY: `h` is a valid stack-allocated `HrtimerImpl`, and `queue` is exclusively owned locally.
        assert!(unsafe {
            rearm_hrtimer_after_callback(&mut h, HRTIMER_RESTART, &mut queue, 265_000_000, 27)
        });
        assert_eq!(queue.earliest(), ptr::addr_of_mut!(h.timer));
        assert_eq!(h.timer.expires, 30);

        let mut stopped = test_hrtimer(next_deadline);
        let mut empty_queue = TimerListQueue::new();
        // SAFETY: `stopped` is a valid stack-allocated `HrtimerImpl`, and `empty_queue` is exclusively owned locally.
        assert!(!unsafe {
            rearm_hrtimer_after_callback(
                &mut stopped,
                HRTIMER_NORESTART,
                &mut empty_queue,
                265_000_000,
                27,
            )
        });
        assert!(empty_queue.head.is_null());

        let mut cancelled = test_hrtimer(next_deadline);
        cancelled.timer.function = Some(hrtimer_timer_fn);
        cancelled.function = Some(test_hrtimer_callback);
        // SAFETY: `cancelled` is a valid stack-allocated `HrtimerImpl` cast to `Hrtimer`.
        assert_eq!(
            unsafe { hrtimer_try_to_cancel(ptr::addr_of_mut!(cancelled).cast::<Hrtimer>()) },
            0
        );
        assert!(cancelled.cancelled);
        let mut cancelled_queue = TimerListQueue::new();
        // SAFETY: `cancelled` is a valid stack-allocated `HrtimerImpl`, and `cancelled_queue` is exclusively owned locally.
        assert!(!unsafe {
            rearm_hrtimer_after_callback(
                &mut cancelled,
                HRTIMER_RESTART,
                &mut cancelled_queue,
                265_000_000,
                27,
            )
        });
        assert!(cancelled_queue.head.is_null());
    }

    /// # Safety
    /// Valid C-ABI callback for test hrtimer.
    unsafe extern "C" fn test_hrtimer_callback(_timer: *mut Hrtimer) -> i32 {
        HRTIMER_RESTART
    }
}
