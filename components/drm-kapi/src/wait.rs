// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Wait-queue and polling support used by DRM drivers.

use alloc::vec::Vec;
use core::{
    cell::UnsafeCell,
    ffi::c_void,
    mem::{MaybeUninit, offset_of},
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

use crate::{
    config::{CONFIG_HZ, EAGAIN, ENODEV, MAX_SCHEDULE_TIMEOUT},
    kernel_services::ProviderSlot,
    list::{INIT_LIST_HEAD, list_add_tail, list_del_init, list_empty},
    locks::{spin_lock_irqsave, spin_unlock_irqrestore},
    types::{WaitQueueEntry, WaitQueueHead},
};

/// Task-context sleep and wake operations supplied by the hosting scheduler.
/// `wait_timeout_until` returns true only when the deadline elapsed and the
/// predicate is still false. `wake_all` may wake unrelated predicates, which
/// are always rechecked before the caller proceeds.
pub struct WaitOps {
    pub wait_timeout_until: fn(timeout_ns: Option<u64>, ready: &dyn Fn() -> bool) -> bool,
    pub wake_all: fn(),
}

static WAIT_OPS: ProviderSlot<WaitOps> = ProviderSlot::new();

/// Install the native wait provider before the first blocking KAPI operation.
pub fn install_wait_ops(ops: &'static WaitOps) -> Result<(), &'static str> {
    WAIT_OPS.install(ops)
}

fn wait_ops() -> &'static WaitOps {
    #[cfg(test)]
    {
        static TEST_OPS: WaitOps = WaitOps {
            wait_timeout_until: test_wait_timeout_until,
            wake_all: || {},
        };
        let _ = WAIT_OPS.install(&TEST_OPS);
    }
    WAIT_OPS.require("wait")
}

/// Whether the host installed a scheduler wait provider.
pub fn wait_ops_installed() -> bool {
    WAIT_OPS.get().is_some()
}

#[cfg(test)]
fn test_wait_timeout_until(timeout_ns: Option<u64>, ready: &dyn Fn() -> bool) -> bool {
    let start = std::time::Instant::now();
    loop {
        if ready() {
            return false;
        }
        if timeout_ns.is_some_and(|timeout| start.elapsed().as_nanos() >= timeout as u128) {
            return true;
        }
        std::thread::yield_now();
    }
}

/// Wake all tasks blocked in host wait predicates. Waiters must recheck their
/// own conditions because notifications are intentionally shared.
pub fn wake_all() {
    (wait_ops().wake_all)();
}

/// `might_sleep()` is compiled out for this Linux 7.2.3 target because
/// CONFIG_DEBUG_ATOMIC_SLEEP is disabled.
#[inline]
pub fn might_sleep() {}

pub const TASK_INTERRUPTIBLE: u32 = 1;
pub const TASK_UNINTERRUPTIBLE: u32 = 2;
pub const TASK_RUNNING: u32 = 0;
pub const WQ_FLAG_EXCLUSIVE: u32 = 1;
pub const WQ_FLAG_WOKEN: u32 = 2;
pub const WQ_FLAG_CUSTOM: u32 = 4;
pub const WQ_FLAG_DONE: u32 = 8;
pub const WQ_FLAG_PRIORITY: u32 = 16;

/// Linux `cond_resched()` voluntary scheduling point.
#[inline]
pub fn cond_resched() {
    crate::clock::relax();
}

/// Linux task state consumed by [`schedule_timeout`]. `TASK_RUNNING` is the
/// absence of a sleep request; a wake that arrives after `set_current_state()`
/// turns the recorded state into `TASK_RUNNING`, so the following schedule
/// returns at once and no wakeup is lost.
struct TaskState {
    task: usize,
    state: u32,
    waiter: usize,
}

static TASK_STATES: spin::Mutex<Vec<TaskState>> = spin::Mutex::new(Vec::new());

#[inline]
fn current_task_key() -> usize {
    crate::kernel_services::current_task_id()
        .and_then(|task| usize::try_from(task).ok())
        .unwrap_or(0)
}

fn set_task_state(task: usize, state: u32) {
    if task == 0 {
        return;
    }
    let mut states = TASK_STATES.lock();
    let index = states.iter().position(|entry| entry.task == task);
    match (index, state) {
        (Some(index), TASK_RUNNING) => {
            states.swap_remove(index);
        }
        (Some(index), _) => states[index].state = state,
        (None, TASK_RUNNING) => {}
        (None, _) => states.push(TaskState {
            task,
            state,
            waiter: 0,
        }),
    }
}

fn peek_task_state(task: usize) -> Option<u32> {
    if task == 0 {
        return None;
    }
    TASK_STATES
        .lock()
        .iter()
        .find(|entry| entry.task == task)
        .map(|entry| entry.state)
}

/// Consume the recorded state of `task`, if any.
fn take_task_state(task: usize) -> Option<u32> {
    if task == 0 {
        return None;
    }
    let mut states = TASK_STATES.lock();
    let index = states.iter().position(|entry| entry.task == task)?;
    Some(states.swap_remove(index).state)
}

/// Publish the current stack wait flag while holding the state lock, so a
/// concurrent wake either sets it or observes that no waiter remains.
fn attach_task_waiter(task: usize, ready: &AtomicBool) -> Option<u32> {
    if task == 0 {
        return None;
    }
    let mut states = TASK_STATES.lock();
    let entry = states.iter_mut().find(|entry| entry.task == task)?;
    entry.waiter = ready as *const AtomicBool as usize;
    if entry.state == TASK_RUNNING {
        ready.store(true, Ordering::Release);
    }
    Some(entry.state)
}

fn detach_task_waiter(task: usize, ready: &AtomicBool) {
    if task == 0 {
        return;
    }
    if let Some(entry) = TASK_STATES
        .lock()
        .iter_mut()
        .find(|entry| entry.task == task && entry.waiter == ready as *const AtomicBool as usize)
    {
        entry.waiter = 0;
    }
}

/// Linux `wake_up_process()`: makes a sleeping task runnable. Returns 1 when
/// the task was sleeping and is now woken, 0 when it was already running.
pub fn wake_up_task(task: usize) -> i32 {
    if task == 0 {
        return 0;
    }
    let mut states = TASK_STATES.lock();
    let woken = match states.iter_mut().find(|entry| entry.task == task) {
        Some(slot) if slot.state != TASK_RUNNING => {
            // Keep a RUNNING record so the pending schedule observes the wake.
            slot.state = TASK_RUNNING;
            if slot.waiter != 0 {
                // SAFETY: the stack flag remains live until schedule_timeout
                // detaches it under this same state lock.
                unsafe { &*(slot.waiter as *const AtomicBool) }.store(true, Ordering::Release);
            }
            true
        }
        _ => false,
    };
    drop(states);
    if woken {
        wake_all();
        1
    } else {
        0
    }
}

/// Linux `schedule_timeout()`. A task that recorded a sleeping state sleeps
/// until `wake_up_task()` or the timeout; a task without a recorded state keeps
/// the timed-sleep behaviour of the non-interruptible wait paths. Returns
/// the remaining jiffies, or 0 when the full timeout elapsed.
pub fn schedule_timeout(timeout: i64) -> i64 {
    if timeout < 0 {
        return 0;
    }
    let task = current_task_key();
    let tick_ns = 1_000_000_000 / CONFIG_HZ as u64;
    let unbounded = timeout as u64 >= MAX_SCHEDULE_TIMEOUT;
    // Linux arms `jiffies + timeout` and returns `expire - jiffies`. Use the
    // same absolute tick deadline so a wait that is woken and re-entered many
    // times does not lose the partial tick of every sleep.
    let start_tick = crate::clock::now_ns() / tick_ns;
    let expire_tick = start_tick.saturating_add(timeout as u64);
    let nanos = if unbounded {
        u64::MAX
    } else {
        expire_tick
            .saturating_mul(tick_ns)
            .saturating_sub(crate::clock::now_ns())
    };
    match peek_task_state(task) {
        Some(TASK_RUNNING) => {
            take_task_state(task);
            return timeout;
        }
        Some(state) => {
            let ready = AtomicBool::new(false);
            let _ = attach_task_waiter(task, &ready);
            // A process signal wakes the native task directly, without passing
            // through this Linux wait queue. Recheck pending signals when the
            // native scheduler re-evaluates the parking predicate, or an
            // interruptible task would immediately park again and lose the
            // signal wake. Uninterruptible waits must ignore ordinary signals.
            let interruptible = state & TASK_INTERRUPTIBLE != 0;
            let predicate = || {
                ready.load(Ordering::Acquire)
                    || (interruptible && crate::signal::signal_pending_current())
            };
            let deadline = (!unbounded).then_some(nanos.min(i64::MAX as u64));
            let _ = wait_timeout_until(deadline, &predicate);
            detach_task_waiter(task, &ready);
            take_task_state(task);
        }
        None => {
            let predicate = || false;
            let deadline = (!unbounded).then_some(nanos.min(i64::MAX as u64));
            let _ = wait_timeout_until(deadline, &predicate);
        }
    }
    if unbounded {
        return 0;
    }
    expire_tick.saturating_sub(crate::clock::now_ns() / tick_ns) as i64
}

/// Linux `schedule_timeout_uninterruptible()`.
///
/// # Safety
/// Call only from a valid kernel task context where the installed scheduler
/// wait provider may block; `timeout` must be a Linux jiffies timeout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn schedule_timeout_uninterruptible(timeout: i64) -> i64 {
    set_task_state(current_task_key(), TASK_UNINTERRUPTIBLE);
    schedule_timeout(timeout)
}

/// Linux `msleep()` backed by the task timer; if called outside a sleepable
/// task context, retain the requested minimum delay via cooperative polling.
pub fn msleep(milliseconds: u32) {
    if milliseconds == 0 {
        return;
    }
    let nanos = u64::from(milliseconds).saturating_mul(1_000_000);
    let predicate = || false;
    let _ = wait_timeout_until(Some(nanos), &predicate);
}

pub type WaitQueueFunc = unsafe extern "C" fn(*mut WaitQueueEntry, u32, i32, *mut c_void) -> i32;

pub fn init_waitqueue_head(head: &mut WaitQueueHead) {
    // SAFETY: head.head is a valid mutable reference to ListHead; addr_of_mut produces an aligned pointer.
    unsafe { INIT_LIST_HEAD(core::ptr::addr_of_mut!(head.head)) };
}

pub fn add_wait_queue(head: &mut WaitQueueHead, entry: &mut WaitQueueEntry) {
    let mut flags = 0u64;
    spin_lock_irqsave(&mut head.lock, &mut flags);
    // SAFETY: head.lock is held, protecting concurrent list mutation, and both entry.entry and head.head are valid references.
    unsafe {
        list_add_tail(
            core::ptr::addr_of_mut!(entry.entry),
            core::ptr::addr_of_mut!(head.head),
        )
    };
    spin_unlock_irqrestore(&mut head.lock, flags);
}

pub fn remove_wait_queue(head: &mut WaitQueueHead, entry: &mut WaitQueueEntry) {
    let mut flags = 0u64;
    spin_lock_irqsave(&mut head.lock, &mut flags);
    // SAFETY: head.lock is held, serializing list removal, and entry.entry is a valid reference.
    unsafe { list_del_init(core::ptr::addr_of_mut!(entry.entry)) };
    spin_unlock_irqrestore(&mut head.lock, flags);
}

pub fn wake_up(head: &mut WaitQueueHead) {
    wake_up_all(head);
}

pub fn wake_up_all(head: &mut WaitQueueHead) {
    let mut flags = 0u64;
    spin_lock_irqsave(&mut head.lock, &mut flags);
    // SAFETY: head.lock is held, preventing concurrent modification; head.head.next is an initialized list pointer.
    let mut node = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(head.head.next)) };
    let sentinel = core::ptr::addr_of_mut!(head.head);
    while !core::ptr::eq(node, sentinel) {
        let entry = node
            .cast::<u8>()
            .wrapping_sub(offset_of!(WaitQueueEntry, entry))
            .cast::<WaitQueueEntry>();
        // SAFETY: node is non-null and not the sentinel, pointing to an active list node protected by head.lock.
        let next = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*node).next)) };
        // SAFETY: entry is calculated from container_of (offset_of) on a valid node and points to an aligned WaitQueueEntry.
        if let Some(wake) = unsafe { (*entry).func } {
            // SAFETY: wake is a valid function pointer called with entry while head.lock is held.
            unsafe { wake(entry, u32::MAX, 0, core::ptr::null_mut()) };
        }
        node = next;
    }
    spin_unlock_irqrestore(&mut head.lock, flags);
}

/// Linux-compatible default wait-queue wake callback.
///
/// # Safety
/// `entry` must be null or point to a live initialized `WaitQueueEntry` whose
/// `private` field is null or contains a valid scheduler task identity token.
pub unsafe extern "C" fn default_wake_function(
    entry: *mut WaitQueueEntry,
    _mode: u32,
    _sync: i32,
    _key: *mut c_void,
) -> i32 {
    // SAFETY: entry validity is guaranteed by the caller's contract and forwarded to woken_wake_function.
    unsafe { woken_wake_function(entry, _mode, _sync, _key) }
}

/// Linux task wake callback used by wait entries initialized by this module.
///
/// # Safety
/// `entry` must be null or point to a live initialized `WaitQueueEntry` whose
/// `private` field is null or contains a scheduler task identity token.
pub unsafe extern "C" fn woken_wake_function(
    entry: *mut WaitQueueEntry,
    _mode: u32,
    _sync: i32,
    _key: *mut c_void,
) -> i32 {
    if entry.is_null() {
        return 0;
    }
    // SAFETY: entry is checked non-null and satisfies caller validity; private is checked non-null before waking task.
    unsafe {
        (*entry).flags |= WQ_FLAG_WOKEN;
        // Linux default_wake_function(): wake the task that owns the entry.
        if !(*entry).private.is_null() {
            wake_up_task((*entry).private as usize);
        }
    }
    1
}

/// Wait until a condition becomes true or its monotonic deadline expires.
/// Return true on timeout, matching Linux `wait_for()` call sites.
pub fn wait_timeout_until(timeout_ns: Option<u64>, ready: &dyn Fn() -> bool) -> bool {
    if crate::task_runtime::can_block_current() {
        (wait_ops().wait_timeout_until)(timeout_ns, ready)
    } else {
        let deadline = timeout_ns.map(|timeout| crate::clock::now_ns().saturating_add(timeout));
        loop {
            if ready() {
                return false;
            }
            if deadline.is_some_and(|deadline| crate::clock::now_ns() >= deadline) {
                return true;
            }
            crate::clock::relax();
        }
    }
}

/// Wait for `ready` or until the optional timeout. Returns true only when the
/// predicate is true at the final check; false means it was false at that
/// check, which can happen if a transient condition cleared after a wake or if
/// the wait timed out. Callers must recheck their predicate before acting.
pub fn wait_until_ready(timeout_ns: Option<u64>, ready: &dyn Fn() -> bool) -> bool {
    if ready() {
        return true;
    }
    let _ = wait_timeout_until(timeout_ns, ready);
    ready()
}

/// Fallible task-context wait for worker initialization paths that must not
/// publish a task which would later panic for a missing scheduler provider.
pub fn try_wait_until_ready(
    timeout_ns: Option<u64>,
    ready: &dyn Fn() -> bool,
) -> Result<bool, i32> {
    if ready() {
        return Ok(true);
    }
    if !WAIT_OPS.get().is_some() {
        return Err(-ENODEV);
    }
    if !crate::task_runtime::can_block_current() {
        return Err(-EAGAIN);
    }
    let _ = (wait_ops().wait_timeout_until)(timeout_ns, ready);
    Ok(ready())
}

/// Wait until a predicate becomes true or its monotonic deadline expires.
/// Returns true on timeout, matching Linux `wait_for()` call sites.
pub fn wait_until(timeout_ns: u64, condition: impl Fn() -> bool, yield_task: bool) -> bool {
    let start = crate::clock::now_ns();
    let deadline = start.saturating_add(timeout_ns);
    if !yield_task || !crate::task_runtime::can_block_current() {
        loop {
            if condition() {
                return false;
            }
            if crate::clock::now_ns() >= deadline {
                return true;
            }
            crate::clock::relax();
        }
    }
    wait_timeout_until(Some(deadline.saturating_sub(start)), &condition)
}

pub fn wait_woken(entry: &mut WaitQueueEntry, _state: u32, timeout: i64) -> i64 {
    if timeout <= 0 {
        return 0;
    }
    let tick_ns = 1_000_000_000 / CONFIG_HZ as u64;
    let timeout_ns = if timeout as u64 >= MAX_SCHEDULE_TIMEOUT {
        u64::MAX
    } else {
        (timeout as u64).saturating_mul(tick_ns)
    };
    let start = crate::clock::now_ns();
    let timed_out = wait_until(timeout_ns, || entry.flags & WQ_FLAG_WOKEN != 0, true);
    if timed_out {
        return 0;
    }
    entry.flags &= !WQ_FLAG_WOKEN;
    let elapsed = crate::clock::now_ns().saturating_sub(start);
    timeout
        .saturating_sub((elapsed / tick_ns.max(1)) as i64)
        .max(1)
}

#[macro_export]
macro_rules! DEFINE_WAIT {
    () => {{
        $crate::types::WaitQueueEntry {
            flags: 0,
            private: core::ptr::null_mut(),
            func: Some($crate::wait::default_wake_function),
            entry: $crate::types::ListHead {
                next: core::ptr::null_mut(),
                prev: core::ptr::null_mut(),
            },
        }
    }};
}

#[macro_export]
macro_rules! DEFINE_WAIT_FUNC {
    ($function:ident) => {{
        $crate::types::WaitQueueEntry {
            flags: 0,
            private: core::ptr::null_mut(),
            func: Some($function),
            entry: $crate::types::ListHead {
                next: core::ptr::null_mut(),
                prev: core::ptr::null_mut(),
            },
        }
    }};
}

pub trait AsWaitQueueHeadPtr {
    fn as_wq_head_ptr(&mut self) -> *mut WaitQueueHead;
}

impl AsWaitQueueHeadPtr for WaitQueueHead {
    fn as_wq_head_ptr(&mut self) -> *mut WaitQueueHead {
        self as *mut _
    }
}

impl AsWaitQueueHeadPtr for *mut WaitQueueHead {
    fn as_wq_head_ptr(&mut self) -> *mut WaitQueueHead {
        *self
    }
}

impl AsWaitQueueHeadPtr for &mut WaitQueueHead {
    fn as_wq_head_ptr(&mut self) -> *mut WaitQueueHead {
        (*self) as *mut _
    }
}

#[macro_export]
macro_rules! wait_event_lock_irq {
    ($wait_queue:expr, $condition:expr, $lock:expr $(,)?) => {{
        while !$condition {
            // SAFETY: Caller passes a valid mutable spinlock reference and manages synchronization invariants.
            unsafe { $crate::locks::spin_unlock_irq(&mut $lock) };
            $crate::clock::relax();
            // SAFETY: Re-acquires the caller-provided spinlock after yielding.
            unsafe { $crate::locks::spin_lock_irq(&mut $lock) };
        }
    }};
}

/// Linux `wait_event_timeout()` macro: sleeps until condition evaluates to true
/// or the timeout in jiffies expires.
///
/// Returns:
/// - 0 if condition evaluated to false after timeout elapsed;
/// - 1 if condition evaluated to true after timeout elapsed;
/// - remaining jiffies (at least 1) if condition evaluated to true before timeout elapsed.
#[macro_export]
macro_rules! wait_event_timeout {
    ($wq:expr, $condition:expr, $timeout:expr $(,)?) => {{
        let mut __ret: i64 = $timeout as i64;
        $crate::wait::might_sleep();
        if !{
            let __cond = $condition;
            if __cond && __ret == 0 {
                __ret = 1;
            }
            __cond || __ret == 0
        } {
            let mut __entry = $crate::DEFINE_WAIT!();
            // SAFETY: __entry is a stack-allocated WaitQueueEntry initialized for local waiting.
            unsafe {
                $crate::wait::init_wait_entry(core::ptr::addr_of_mut!(__entry), 0);
            }
            let __wq_ptr = $crate::wait::AsWaitQueueHeadPtr::as_wq_head_ptr(&mut ($wq));
            loop {
                // SAFETY: __wq_ptr points to the wait queue and __entry is a valid local wait entry.
                let _ = unsafe {
                    $crate::wait::prepare_to_wait_event(
                        __wq_ptr,
                        core::ptr::addr_of_mut!(__entry),
                        $crate::wait::TASK_UNINTERRUPTIBLE as i32,
                    )
                };
                let __cond = $condition;
                if __cond && __ret == 0 {
                    __ret = 1;
                }
                if __cond || __ret == 0 {
                    break;
                }
                __ret = $crate::wait::schedule_timeout(__ret);
                let __cond = $condition;
                if __cond && __ret == 0 {
                    __ret = 1;
                }
                if __cond || __ret == 0 {
                    break;
                }
            }
            // SAFETY: __wq_ptr and __entry point to live objects; deregisters the wait entry before returning.
            unsafe {
                $crate::wait::finish_wait(__wq_ptr, core::ptr::addr_of_mut!(__entry));
            }
        }
        __ret
    }};
}

/// Linux `wait_event_interruptible_timeout()` macro.
#[macro_export]
macro_rules! wait_event_interruptible_timeout {
    ($wq:expr, $condition:expr, $timeout:expr $(,)?) => {{
        let mut __ret: i64 = $timeout as i64;
        $crate::wait::might_sleep();
        if !{
            let __cond = $condition;
            if __cond && __ret == 0 {
                __ret = 1;
            }
            __cond || __ret == 0
        } {
            let mut __entry = $crate::DEFINE_WAIT!();
            // SAFETY: __entry is a stack-allocated WaitQueueEntry initialized for local waiting.
            unsafe {
                $crate::wait::init_wait_entry(core::ptr::addr_of_mut!(__entry), 0);
            }
            let __wq_ptr = $crate::wait::AsWaitQueueHeadPtr::as_wq_head_ptr(&mut ($wq));
            loop {
                // SAFETY: __wq_ptr points to the wait queue and __entry is a valid local wait entry.
                let __int = unsafe {
                    $crate::wait::prepare_to_wait_event(
                        __wq_ptr,
                        core::ptr::addr_of_mut!(__entry),
                        $crate::wait::TASK_INTERRUPTIBLE as i32,
                    )
                };
                let __cond = $condition;
                if __cond && __ret == 0 {
                    __ret = 1;
                }
                if __cond || __ret == 0 {
                    break;
                }
                if __int != 0 {
                    __ret = __int;
                    break;
                }
                __ret = $crate::wait::schedule_timeout(__ret);
                let __cond = $condition;
                if __cond && __ret == 0 {
                    __ret = 1;
                }
                if __cond || __ret == 0 {
                    break;
                }
            }
            // SAFETY: __wq_ptr and __entry point to live objects; deregisters the wait entry before returning.
            unsafe {
                $crate::wait::finish_wait(__wq_ptr, core::ptr::addr_of_mut!(__entry));
            }
        }
        __ret
    }};
}

/// Linux `wait_event_interruptible()` macro.
#[macro_export]
macro_rules! wait_event_interruptible {
    ($wq:expr, $condition:expr $(,)?) => {{
        let __res = $crate::wait_event_interruptible_timeout!(
            $wq,
            $condition,
            $crate::config::MAX_SCHEDULE_TIMEOUT as i64,
        );
        if __res < 0 { __res as i32 } else { 0i32 }
    }};
}

pub type PollQueueProc =
    Option<unsafe extern "C" fn(*mut crate::types::File, *mut WaitQueueHead, *mut PollTable)>;

#[repr(C)]
pub struct PollTable {
    pub qproc: PollQueueProc,
    pub key: u32,
}

/// Linux `poll_wait()`: registers a wait queue with a poll table.
///
/// # Safety
/// Each non-null pointer must refer to a live object of its declared type;
/// `wait_address` must remain valid while the poll registration may be used.
#[inline]
pub unsafe fn poll_wait(
    filp: *mut crate::types::File,
    wait_address: *mut WaitQueueHead,
    p: *mut PollTable,
) {
    // SAFETY: p is non-null and points to an aligned PollTable struct.
    if !p.is_null()
        && !wait_address.is_null()
        && let Some(qproc) = unsafe { (*p).qproc }
    {
        // SAFETY: qproc is a valid function pointer called with live filp, wait_address, and p pointers.
        unsafe { qproc(filp, wait_address, p) };
    }
}

/// Linux `wake_up_interruptible_poll()`.
#[inline]
pub fn wake_up_interruptible_poll(head: &mut WaitQueueHead, _key: u32) {
    wake_up_all(head);
}

/// Linux `wait_event_interruptible_locked()` macro.
#[macro_export]
macro_rules! wait_event_interruptible_locked {
    ($wq:expr, $condition:expr $(,)?) => {{
        if $condition {
            0i32
        } else {
            let mut __wait = $crate::DEFINE_WAIT!();
            // SAFETY: __wait is a stack-allocated WaitQueueEntry initialized for local waiting.
            unsafe {
                $crate::wait::init_wait_entry(core::ptr::addr_of_mut!(__wait), 0);
            }
            let __wq_ptr = $crate::wait::AsWaitQueueHeadPtr::as_wq_head_ptr(&mut ($wq));
            let mut __ret = 0i32;
            loop {
                // SAFETY: __wq_ptr and __wait point to valid wait queue objects.
                __ret = unsafe {
                    $crate::wait::do_wait_intr(__wq_ptr, core::ptr::addr_of_mut!(__wait))
                };
                if __ret != 0 {
                    break;
                }
                if $condition {
                    break;
                }
            }
            // SAFETY: __wq_ptr and __wait point to live objects; removes the entry and restores running task state.
            unsafe {
                $crate::wait::__remove_wait_queue(__wq_ptr, core::ptr::addr_of_mut!(__wait));
                $crate::wait::__set_current_state($crate::wait::TASK_RUNNING as i32);
            }
            __ret
        }
    }};
}

// ---------------------------------------------------------------------------
// C ABI entry points for the scheduler and wait-queue primitives.
// ---------------------------------------------------------------------------

/// Linux `set_current_state()`.
#[unsafe(no_mangle)]
pub extern "C" fn set_current_state(state: i32) {
    set_task_state(current_task_key(), state as u32);
}

/// Linux `__set_current_state()`: the same store without the barrier.
#[unsafe(no_mangle)]
pub extern "C" fn __set_current_state(state: i32) {
    set_task_state(current_task_key(), state as u32);
}

/// Linux `need_resched()`: whether the current task has a pending reschedule.
/// The request is only read here; it is consumed by the scheduler at the next
/// preemption point, as in Linux.
#[unsafe(no_mangle)]
pub extern "C" fn need_resched() -> bool {
    crate::task_runtime::current_need_resched()
}

/// Linux `might_sleep()`: compiled out with CONFIG_DEBUG_ATOMIC_SLEEP off.
#[unsafe(export_name = "might_sleep")]
pub extern "C" fn c_might_sleep() {}

/// Linux `wake_up_process()`.
#[unsafe(no_mangle)]
pub extern "C" fn wake_up_process(task: *mut c_void) -> i32 {
    wake_up_task(task as usize)
}

/// Linux `schedule_timeout()` C entry point (the Rust helper keeps its name).
#[unsafe(no_mangle)]
pub extern "C" fn io_schedule_timeout(timeout: i64) -> i64 {
    schedule_timeout(timeout)
}

/// Linux `prepare_to_wait()`: queue `entry` on `wq` if it is not queued yet and
/// record the sleeping state of the current task.
///
/// # Safety
/// `wq` must be an initialized wait queue and `entry` must be writable,
/// initialized storage that remains live until `finish_wait()` removes it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prepare_to_wait(
    wq: *const WaitQueueHead,
    entry: *mut WaitQueueEntry,
    state: i32,
) {
    assert!(!wq.is_null() && !entry.is_null());
    // SAFETY: wq and entry are checked non-null and caller guarantees their validity and lifetime.
    unsafe {
        (*entry).flags &= !WQ_FLAG_EXCLUSIVE;
        if (*entry).private.is_null() {
            (*entry).private = current_task_key() as *mut c_void;
        }
        if list_empty(&(*entry).entry) {
            queue_entry_locked(wq.cast_mut(), entry, false);
        }
    }
    set_task_state(current_task_key(), state as u32);
}

/// Linux `finish_wait()`: restore TASK_RUNNING and dequeue `entry` if queued.
///
/// # Safety
/// `wq` and `entry` must be live initialized objects, and `entry` must not be
/// concurrently removed or reused before this call completes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finish_wait(wq: *const WaitQueueHead, entry: *mut WaitQueueEntry) {
    assert!(!wq.is_null() && !entry.is_null());
    set_task_state(current_task_key(), TASK_RUNNING);
    // SAFETY: entry is verified non-null and points to an aligned WaitQueueEntry.
    let next = unsafe { (*entry).entry.next };
    // SAFETY: entry is valid storage with an initialized ListHead.
    if !next.is_null() && !list_empty(unsafe { &(*entry).entry }) {
        let mut flags = 0u64;
        let head = wq.cast_mut();
        // SAFETY: head is non-null and points to an initialized WaitQueueHead.
        unsafe { spin_lock_irqsave(&mut (*head).lock, &mut flags) };
        // SAFETY: head.lock is held and entry.entry is part of the wait queue list.
        unsafe { list_del_init(core::ptr::addr_of_mut!((*entry).entry)) };
        // SAFETY: head.lock was acquired with spin_lock_irqsave with matching flags.
        unsafe { spin_unlock_irqrestore(&mut (*head).lock, flags) };
    }
}

/// Linux `add_wait_queue()`: clears the exclusive flag and inserts at the head
/// of the list, as `__add_wait_queue()` does.
///
/// # Safety
/// `wq` must be initialized and `entry` must be writable, initialized storage
/// not linked to another queue; it must remain live until removal.
#[unsafe(export_name = "add_wait_queue")]
pub unsafe extern "C" fn c_add_wait_queue(wq: *mut WaitQueueHead, entry: *mut WaitQueueEntry) {
    // SAFETY: wq and entry satisfy c_add_wait_queue safety preconditions.
    unsafe {
        (*entry).flags &= !WQ_FLAG_EXCLUSIVE;
        queue_entry_locked(wq, entry, true);
    }
}

/// Insert `entry` under the wait-queue spinlock, at the head or the tail.
///
/// # Safety
/// `wq` must point to an initialized `WaitQueueHead` and `entry` must point to a live `WaitQueueEntry` not linked to another list.
unsafe fn queue_entry_locked(wq: *mut WaitQueueHead, entry: *mut WaitQueueEntry, head: bool) {
    let mut flags = 0u64;
    // SAFETY: wq points to an initialized WaitQueueHead.
    unsafe { spin_lock_irqsave(&mut (*wq).lock, &mut flags) };
    // SAFETY: wq.lock is held, wq.head is the queue sentinel, and entry.entry is valid storage.
    unsafe {
        let list = core::ptr::addr_of_mut!((*wq).head);
        let node = core::ptr::addr_of_mut!((*entry).entry);
        if head {
            crate::list::list_add(node, list);
        } else {
            list_add_tail(node, list);
        }
    }
    // SAFETY: wq.lock was acquired with spin_lock_irqsave with matching flags.
    unsafe { spin_unlock_irqrestore(&mut (*wq).lock, flags) };
}

/// Wait-table size for `__var_waitqueue()` (Linux uses 256 buckets).
const VAR_WAIT_BUCKETS: usize = 64;

struct VarWaitSlot {
    /// 0 = uninitialised, 1 = being initialised, 2 = ready.
    state: AtomicU8,
    head: UnsafeCell<MaybeUninit<WaitQueueHead>>,
}

// SAFETY: the head is written once under the `state` handshake and afterwards
// accessed only through its own spinlock.
unsafe impl Sync for VarWaitSlot {}

static VAR_WAIT_TABLE: [VarWaitSlot; VAR_WAIT_BUCKETS] = [const {
    VarWaitSlot {
        state: AtomicU8::new(0),
        head: UnsafeCell::new(MaybeUninit::zeroed()),
    }
}; VAR_WAIT_BUCKETS];

/// Linux `__var_waitqueue()`: the wait queue hashed from an address.
#[unsafe(no_mangle)]
pub extern "C" fn __var_waitqueue(addr: *mut c_void) -> *mut WaitQueueHead {
    let hashed = (addr as usize >> 3).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58;
    let slot = &VAR_WAIT_TABLE[hashed % VAR_WAIT_BUCKETS];
    loop {
        match slot
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                // SAFETY: slot.state was transitioned to 1 (being initialized) exclusively by this thread, guaranteeing unique mutable access to slot.head.
                let head = unsafe { (*slot.head.get()).as_mut_ptr() };
                // SAFETY: head is uniquely accessed during initialization and points to an aligned WaitQueueHead buffer.
                init_waitqueue_head(unsafe { &mut *head });
                slot.state.store(2, Ordering::Release);
                break;
            }
            Err(2) => break,
            Err(_) => core::hint::spin_loop(),
        }
    }
    // SAFETY: slot.state is 2 (ready), so head is fully initialized and thread-safe via internal spinlock.
    unsafe { (*slot.head.get()).as_mut_ptr() }
}

/// Linux `wake_up_var()`.
///
/// # Safety
/// `addr` must be a stable identity address for as long as any waiter is
/// registered against it. This function does not dereference the address.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wake_up_var(addr: *mut c_void) {
    // SAFETY: __var_waitqueue returns an initialized WaitQueueHead pointer for addr.
    unsafe { wake_up_all(&mut *__var_waitqueue(addr)) };
}

// upstream: kernel/sched/wait.c init_wait_entry()
/// Initialize a wait entry for one task and queue.
///
/// # Safety
/// `wq_entry` must point to writable storage that is not currently linked into
/// any wait queue.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn init_wait_entry(wq_entry: *mut WaitQueueEntry, flags: i32) {
    if !wq_entry.is_null() {
        // SAFETY: wq_entry is non-null and points to valid writable storage for WaitQueueEntry.
        unsafe {
            (*wq_entry).flags = flags as u32;
            (*wq_entry).private = current_task_key() as *mut c_void;
            (*wq_entry).func = Some(default_wake_function);
            INIT_LIST_HEAD(core::ptr::addr_of_mut!((*wq_entry).entry));
        }
    }
}

// upstream: kernel/sched/wait.c prepare_to_wait_event()
/// Prepare an event wait, respecting exclusive queue membership and signals.
///
/// # Safety
/// `wq_head` must be an initialized queue and `wq_entry` must be live writable
/// storage initialized by `init_wait_entry` and not linked elsewhere.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn prepare_to_wait_event(
    wq_head: *mut WaitQueueHead,
    wq_entry: *mut WaitQueueEntry,
    state: i32,
) -> i64 {
    assert!(!wq_head.is_null() && !wq_entry.is_null());
    let mut flags = 0u64;
    // SAFETY: wq_head is non-null and points to an initialized WaitQueueHead.
    unsafe { spin_lock_irqsave(&mut (*wq_head).lock, &mut flags) };
    let mut ret = 0i64;
    // SAFETY: wq_entry is non-null, aligned, and live storage for WaitQueueEntry.
    unsafe {
        if (*wq_entry).private.is_null() {
            (*wq_entry).private = current_task_key() as *mut c_void;
        }
        if (*wq_entry).func.is_none() {
            (*wq_entry).func = Some(default_wake_function);
        }
    }
    let pending =
        if (state as u32 & (TASK_INTERRUPTIBLE | TASK_UNINTERRUPTIBLE)) == TASK_INTERRUPTIBLE {
            crate::signal::signal_pending_current()
        } else {
            false
        };
    if pending {
        // SAFETY: wq_head.lock is held, protecting list removal of wq_entry.entry.
        unsafe { list_del_init(core::ptr::addr_of_mut!((*wq_entry).entry)) };
        ret = -crate::config::ERESTARTSYS as i64;
    } else {
        // SAFETY: wq_head.lock is held; wq_entry and wq_head point to valid initialized queue structures.
        unsafe {
            if (*wq_entry).entry.next.is_null() {
                INIT_LIST_HEAD(core::ptr::addr_of_mut!((*wq_entry).entry));
            }
            if list_empty(&(*wq_entry).entry) {
                if (*wq_entry).flags & WQ_FLAG_EXCLUSIVE != 0 {
                    list_add_tail(
                        core::ptr::addr_of_mut!((*wq_entry).entry),
                        core::ptr::addr_of_mut!((*wq_head).head),
                    );
                } else {
                    crate::list::list_add(
                        core::ptr::addr_of_mut!((*wq_entry).entry),
                        core::ptr::addr_of_mut!((*wq_head).head),
                    );
                }
            }
        }
        set_task_state(current_task_key(), state as u32);
    }
    // SAFETY: wq_head.lock was acquired with spin_lock_irqsave with matching flags.
    unsafe { spin_unlock_irqrestore(&mut (*wq_head).lock, flags) };
    ret
}

// upstream: kernel/sched/wait.c do_wait_intr()
/// Drop the caller-held wait-queue lock, yield, then reacquire it.
///
/// # Safety
/// `wq` must be initialized and its lock must be held by the caller; `wait`
/// must be live initialized storage for a wait entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn do_wait_intr(
    wq: *mut WaitQueueHead,
    wait: *mut WaitQueueEntry,
) -> core::ffi::c_int {
    assert!(!wq.is_null() && !wait.is_null());
    // SAFETY: wait and wq are non-null and live; caller holds wq.lock.
    unsafe {
        if (*wait).entry.next.is_null() {
            INIT_LIST_HEAD(core::ptr::addr_of_mut!((*wait).entry));
        }
        if list_empty(&(*wait).entry) {
            list_add_tail(
                core::ptr::addr_of_mut!((*wait).entry),
                core::ptr::addr_of_mut!((*wq).head),
            );
        }
    }
    set_task_state(current_task_key(), TASK_INTERRUPTIBLE);
    if crate::signal::signal_pending_current() {
        return -crate::config::ERESTARTSYS;
    }
    // SAFETY: wq is non-null and its lock was held by caller; spin_unlock/spin_lock safely yields.
    unsafe {
        crate::locks::spin_unlock(&mut (*wq).lock);
        crate::clock::relax();
        crate::locks::spin_lock(&mut (*wq).lock);
    }
    0
}

// upstream: kernel/sched/wait.c do_wait_intr_irq()
/// IRQ-saving counterpart of [`do_wait_intr`].
///
/// # Safety
/// `wq` must be initialized and its lock must be held with local IRQ state
/// matching `spin_unlock_irq`/`spin_lock_irq`; `wait` must be a live entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn do_wait_intr_irq(
    wq: *mut WaitQueueHead,
    wait: *mut WaitQueueEntry,
) -> core::ffi::c_int {
    assert!(!wq.is_null() && !wait.is_null());
    // SAFETY: wait and wq are non-null and live; caller holds wq.lock with irqs disabled.
    unsafe {
        if (*wait).entry.next.is_null() {
            INIT_LIST_HEAD(core::ptr::addr_of_mut!((*wait).entry));
        }
        if list_empty(&(*wait).entry) {
            list_add_tail(
                core::ptr::addr_of_mut!((*wait).entry),
                core::ptr::addr_of_mut!((*wq).head),
            );
        }
    }
    set_task_state(current_task_key(), TASK_INTERRUPTIBLE);
    if crate::signal::signal_pending_current() {
        return -crate::config::ERESTARTSYS;
    }
    // SAFETY: wq is non-null; caller holds wq.lock with irqs disabled; spin_unlock_irq/spin_lock_irq safely yields.
    unsafe {
        crate::locks::spin_unlock_irq(&mut (*wq).lock);
        crate::clock::relax();
        crate::locks::spin_lock_irq(&mut (*wq).lock);
    }
    0
}

// upstream: include/linux/wait.h __remove_wait_queue()
/// Remove a live wait entry from its owning queue.
///
/// # Safety
/// `wait` must be null or point to a live initialized entry; if linked, its
/// list links must be valid and no other caller may remove it concurrently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __remove_wait_queue(_wq: *mut WaitQueueHead, wait: *mut WaitQueueEntry) {
    if !wait.is_null() {
        // SAFETY: wait is non-null and caller guarantees its list links are valid and not concurrently removed.
        unsafe { list_del_init(core::ptr::addr_of_mut!((*wait).entry)) };
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn wait_until_ready_reports_a_transient_condition_clearing() {
        let checks = AtomicUsize::new(0);
        let transient = || checks.fetch_add(1, Ordering::Relaxed) == 1;

        // The initial check sees false; the wait observes true; the final
        // check sees that the condition has already cleared.
        assert!(!wait_until_ready(None, &transient));
        assert_eq!(checks.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_wait_event_timeout_contract() {
        let mut wq = WaitQueueHead {
            lock: crate::types::Spinlock::new(),
            head: crate::types::ListHead {
                next: core::ptr::null_mut(),
                prev: core::ptr::null_mut(),
            },
        };
        init_waitqueue_head(&mut wq);

        // 1. Condition already true: returns remaining jiffies (timeout >= 1)
        let ret = wait_event_timeout!(wq, true, 100);
        assert_eq!(ret, 100);

        // 2. Condition remains false across timeout: returns 0
        let ret = wait_event_timeout!(wq, false, 1);
        assert_eq!(ret, 0);

        // 3. Condition becomes true at timeout: returns 1
        let mut checked = false;
        let ret = wait_event_timeout!(
            wq,
            {
                if checked {
                    true
                } else {
                    checked = true;
                    false
                }
            },
            1
        );
        assert_eq!(ret, 1);

        // 4. Interruptible timeout variant
        let ret_intr = wait_event_interruptible_timeout!(wq, true, 50);
        assert_eq!(ret_intr, 50);
        let ret_intr_to = wait_event_interruptible_timeout!(wq, false, 1);
        assert_eq!(ret_intr_to, 0);

        // 5. Interruptible locked variant
        let ret_locked = wait_event_interruptible_locked!(wq, true);
        assert_eq!(ret_locked, 0);
        let mut locked_checked = false;
        let ret_locked2 = wait_event_interruptible_locked!(wq, {
            if locked_checked {
                true
            } else {
                locked_checked = true;
                false
            }
        });
        assert_eq!(ret_locked2, 0);
    }
}
