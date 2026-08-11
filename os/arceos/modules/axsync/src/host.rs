//! Host-only synchronization engine used by portable component tests.

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
#[cfg(feature = "sleep")]
use core::{
    ptr,
    sync::atomic::{AtomicPtr, AtomicU64},
};
#[cfg(feature = "sleep")]
use std::{boxed::Box, sync::Condvar};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    sync::{Mutex as StdMutex, OnceLock},
    vec::Vec,
};

use crate::interface::{
    AcquireResult, CONTEXT_IRQSAVE, CONTEXT_PREEMPT, CONTEXT_PREEMPT_IRQSAVE, CONTEXT_RAW,
    LOCK_MODE_EXCLUSIVE, LOCK_MODE_READ, LOCK_MODE_WRITE, LockMetadata,
};

const READER: usize = 1;
const WRITER: usize = 1 << (usize::BITS - 1);
const MAX_READER: usize = 1 << (usize::BITS - 2);

std::thread_local! {
    static PREEMPT_DEPTH: Cell<usize> = const { Cell::new(0) };
    static IRQ_ENABLED: Cell<bool> = const { Cell::new(true) };
    static HELD_LOCKS: RefCell<Vec<HeldLock>> = const { RefCell::new(Vec::new()) };
}

#[cfg(feature = "sleep")]
std::thread_local! {
    static TASK_ID: Cell<u64> = const { Cell::new(0) };
    static MIGHT_SLEEP_CALLS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(feature = "sleep")]
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);
static TRACE_ENABLED: AtomicBool = AtomicBool::new(false);
static LOCK_GRAPH: OnceLock<StdMutex<BTreeSet<(usize, usize)>>> = OnceLock::new();

#[derive(Clone, Copy, Eq, PartialEq)]
enum LockMode {
    Exclusive,
    Read,
    Write,
}

impl LockMode {
    fn from_raw(mode: u8) -> Self {
        match mode {
            LOCK_MODE_EXCLUSIVE => Self::Exclusive,
            LOCK_MODE_READ => Self::Read,
            LOCK_MODE_WRITE => Self::Write,
            _ => panic!("unknown lock ownership mode {mode}"),
        }
    }

    fn permits_nested_read(self, requested: Self) -> bool {
        self == Self::Read && requested == Self::Read
    }
}

#[derive(Clone, Copy)]
struct HeldLock {
    class: usize,
    addr: usize,
    mode: LockMode,
    sleep_forbidden: bool,
}

struct LockRequest<'a> {
    metadata: &'a LockMetadata,
    addr: usize,
    mode: u8,
    subclass: u32,
    sleep_forbidden: bool,
    caller: &'static Location<'static>,
}

struct PendingLockdepAcquire {
    held_before: Vec<HeldLock>,
    requested: HeldLock,
    nested_read: bool,
}

impl PendingLockdepAcquire {
    fn prepare(request: LockRequest<'_>) -> Self {
        let mode = LockMode::from_raw(request.mode);
        let class_key = request.metadata.class_key().load(Ordering::Acquire) as usize;
        let class = class_key
            .wrapping_mul(usize::from(u16::MAX) + 2)
            .wrapping_add(request.subclass as usize);
        let held_before = HELD_LOCKS.with_borrow(Clone::clone);

        let nested_read = held_before
            .iter()
            .find(|held| held.addr == request.addr)
            .is_some_and(|held| held.mode.permits_nested_read(mode));
        if held_before
            .iter()
            .any(|held| held.addr == request.addr && !held.mode.permits_nested_read(mode))
        {
            panic!(
                "lockdep: recursive acquisition at {}:{}",
                request.caller.file(),
                request.caller.line()
            );
        }
        if !request.sleep_forbidden && held_before.iter().any(|held| held.sleep_forbidden) {
            panic!(
                "lockdep: sleeping lock acquired while a non-sleeping lock is held at {}:{}",
                request.caller.file(),
                request.caller.line()
            );
        }

        let inverted = {
            let graph = lock_graph().lock().expect("host lock graph poisoned");
            held_before
                .iter()
                .any(|held| graph.contains(&(class, held.class)))
        };
        if inverted {
            panic!(
                "lockdep: lock order inversion at {}:{}",
                request.caller.file(),
                request.caller.line()
            );
        }

        Self {
            held_before,
            requested: HeldLock {
                class,
                addr: request.addr,
                mode,
                sleep_forbidden: request.sleep_forbidden,
            },
            nested_read,
        }
    }

    fn finish(self, acquired: bool) {
        if !acquired {
            return;
        }
        if !self.nested_read {
            let mut graph = lock_graph().lock().expect("host lock graph poisoned");
            for held in &self.held_before {
                graph.insert((held.class, self.requested.class));
            }
        }
        HELD_LOCKS.with_borrow_mut(|held| held.push(self.requested));
    }
}

fn lock_graph() -> &'static StdMutex<BTreeSet<(usize, usize)>> {
    LOCK_GRAPH.get_or_init(|| StdMutex::new(BTreeSet::new()))
}

fn release_lockdep(addr: usize) {
    HELD_LOCKS.with_borrow_mut(|held| {
        let released = held
            .pop()
            .expect("lockdep: release with an empty held-lock stack");
        assert_eq!(
            released.addr, addr,
            "lockdep: locks must be released in reverse acquisition order"
        );
    });
}

fn force_release_lockdep(addr: usize) {
    HELD_LOCKS.with_borrow_mut(|held| {
        let index = held
            .iter()
            .rposition(|lock| lock.addr == addr)
            .expect("lockdep: force release of an unheld lock");
        held.remove(index);
    });
}

fn disable_preempt() {
    PREEMPT_DEPTH.set(PREEMPT_DEPTH.get() + 1);
}

fn enable_preempt() {
    PREEMPT_DEPTH.set(
        PREEMPT_DEPTH
            .get()
            .checked_sub(1)
            .expect("unbalanced host preemption guard"),
    );
}

pub(crate) fn context_enter(context: u8) -> usize {
    match context {
        CONTEXT_RAW => 0,
        CONTEXT_PREEMPT => {
            disable_preempt();
            0
        }
        CONTEXT_IRQSAVE => usize::from(IRQ_ENABLED.replace(false)),
        CONTEXT_PREEMPT_IRQSAVE => {
            disable_preempt();
            usize::from(IRQ_ENABLED.replace(false))
        }
        _ => panic!("unknown lock context mode {context}"),
    }
}

pub(crate) fn context_exit(context: u8, state: usize) {
    match context {
        CONTEXT_RAW => {}
        CONTEXT_PREEMPT => enable_preempt(),
        CONTEXT_IRQSAVE => IRQ_ENABLED.set(state != 0),
        CONTEXT_PREEMPT_IRQSAVE => {
            IRQ_ENABLED.set(state != 0);
            enable_preempt();
        }
        _ => panic!("unknown lock context mode {context}"),
    }
}

/// Returns the host engine's current preemption nesting depth.
pub fn host_preempt_depth() -> usize {
    PREEMPT_DEPTH.get()
}

#[cfg(test)]
pub(crate) fn host_context_snapshot() -> (usize, bool) {
    (PREEMPT_DEPTH.get(), IRQ_ENABLED.get())
}

struct PendingContext {
    context: u8,
    state: usize,
    armed: bool,
}

impl PendingContext {
    fn enter(context: u8) -> Self {
        Self {
            context,
            state: context_enter(context),
            armed: true,
        }
    }

    fn disarm(mut self) -> usize {
        self.armed = false;
        self.state
    }
}

impl Drop for PendingContext {
    fn drop(&mut self) {
        if self.armed {
            context_exit(self.context, self.state);
        }
    }
}

struct PendingSpinAcquire<'a> {
    locked: &'a AtomicBool,
    acquired: bool,
}

impl Drop for PendingSpinAcquire<'_> {
    fn drop(&mut self) {
        if self.acquired {
            self.locked.store(false, Ordering::Release);
        }
    }
}

pub(crate) fn spin_acquire(
    locked: &AtomicBool,
    metadata: &LockMetadata,
    lock_addr: usize,
    context: u8,
    subclass: u32,
    is_try: bool,
    caller: &'static Location<'static>,
) -> AcquireResult {
    let pending_context = PendingContext::enter(context);
    let lockdep = PendingLockdepAcquire::prepare(LockRequest {
        metadata,
        addr: lock_addr,
        mode: LOCK_MODE_EXCLUSIVE,
        subclass,
        sleep_forbidden: true,
        caller,
    });
    let acquired = if is_try {
        locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    } else {
        loop {
            if locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break true;
            }
            while locked.load(Ordering::Acquire) {
                core::hint::spin_loop();
                std::thread::yield_now();
            }
        }
    };
    let mut pending_lock = PendingSpinAcquire { locked, acquired };
    lockdep.finish(acquired);
    if acquired {
        pending_lock.acquired = false;
        AcquireResult::new(true, pending_context.disarm())
    } else {
        AcquireResult::new(false, 0)
    }
}

pub(crate) fn spin_release(
    locked: &AtomicBool,
    lock_addr: usize,
    context: u8,
    context_state: usize,
) {
    release_lockdep(lock_addr);
    locked.store(false, Ordering::Release);
    context_exit(context, context_state);
}

pub(crate) fn spin_force_release(locked: &AtomicBool, lock_addr: usize, _context: u8) {
    force_release_lockdep(lock_addr);
    locked.store(false, Ordering::Release);
}

pub(crate) fn spin_is_locked(locked: &AtomicBool) -> bool {
    locked.load(Ordering::Acquire)
}

struct PendingRwAcquire<'a> {
    state: &'a AtomicUsize,
    mode: Option<u8>,
}

impl Drop for PendingRwAcquire<'_> {
    fn drop(&mut self) {
        match self.mode {
            Some(LOCK_MODE_READ) => {
                self.state.fetch_sub(READER, Ordering::Release);
            }
            Some(LOCK_MODE_WRITE) => {
                self.state.fetch_and(!WRITER, Ordering::Release);
            }
            Some(mode) => panic!("unknown rwlock rollback mode {mode}"),
            None => {}
        }
    }
}

fn try_read(state: &AtomicUsize) -> bool {
    let old = state.fetch_add(READER, Ordering::Acquire);
    if old & (WRITER | MAX_READER) == 0 {
        true
    } else {
        state.fetch_sub(READER, Ordering::Release);
        false
    }
}

fn try_write(state: &AtomicUsize) -> bool {
    state
        .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

pub(crate) fn rwlock_acquire(
    state: &AtomicUsize,
    metadata: &LockMetadata,
    lock_addr: usize,
    context: u8,
    mode: u8,
    is_try: bool,
    caller: &'static Location<'static>,
) -> AcquireResult {
    let pending_context = PendingContext::enter(context);
    let lockdep = PendingLockdepAcquire::prepare(LockRequest {
        metadata,
        addr: lock_addr,
        mode,
        subclass: 0,
        sleep_forbidden: true,
        caller,
    });
    let acquire_once = || match mode {
        LOCK_MODE_READ => try_read(state),
        LOCK_MODE_WRITE => try_write(state),
        _ => panic!("unknown rwlock acquisition mode {mode}"),
    };
    let acquired = if is_try {
        acquire_once()
    } else {
        loop {
            if acquire_once() {
                break true;
            }
            while match mode {
                LOCK_MODE_READ => state.load(Ordering::Acquire) & WRITER != 0,
                LOCK_MODE_WRITE => state.load(Ordering::Acquire) != 0,
                _ => unreachable!(),
            } {
                core::hint::spin_loop();
                std::thread::yield_now();
            }
        }
    };
    let mut pending_lock = PendingRwAcquire {
        state,
        mode: acquired.then_some(mode),
    };
    lockdep.finish(acquired);
    if acquired {
        pending_lock.mode = None;
        AcquireResult::new(true, pending_context.disarm())
    } else {
        AcquireResult::new(false, 0)
    }
}

pub(crate) fn rwlock_release(
    state: &AtomicUsize,
    lock_addr: usize,
    context: u8,
    context_state: usize,
    mode: u8,
) {
    release_lockdep(lock_addr);
    match mode {
        LOCK_MODE_READ => {
            state.fetch_sub(READER, Ordering::Release);
        }
        LOCK_MODE_WRITE => {
            state.fetch_and(!WRITER, Ordering::Release);
        }
        _ => panic!("unknown rwlock release mode {mode}"),
    }
    context_exit(context, context_state);
}

pub(crate) fn rwlock_force_read_decrement(state: &AtomicUsize, lock_addr: usize, _context: u8) {
    let mut observed = state.load(Ordering::Acquire);
    loop {
        let readers = observed & !(WRITER | MAX_READER);
        if readers == 0 {
            return;
        }
        match state.compare_exchange_weak(
            observed,
            observed - READER,
            Ordering::Release,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                force_release_lockdep(lock_addr);
                return;
            }
            Err(current) => observed = current,
        }
    }
}

#[cfg(feature = "sleep")]
struct HostWaitQueue {
    state: StdMutex<()>,
    condvar: Condvar,
    waiters: AtomicUsize,
}

#[cfg(feature = "sleep")]
impl HostWaitQueue {
    fn new() -> Self {
        Self {
            state: StdMutex::new(()),
            condvar: Condvar::new(),
            waiters: AtomicUsize::new(0),
        }
    }
}

#[cfg(feature = "sleep")]
fn current_task_id() -> u64 {
    TASK_ID.with(|task_id| match task_id.get() {
        0 => {
            let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
            assert_ne!(id, 0, "host task id space exhausted");
            task_id.set(id);
            id
        }
        id => id,
    })
}

#[cfg(feature = "sleep")]
fn ensure_wait_queue(slot: &AtomicPtr<()>) -> &HostWaitQueue {
    let existing = slot.load(Ordering::Acquire).cast::<HostWaitQueue>();
    if !existing.is_null() {
        // SAFETY: the pointer remains owned by the mutex until its exclusive drop.
        return unsafe { &*existing };
    }

    let candidate = Box::into_raw(Box::new(HostWaitQueue::new()));
    match slot.compare_exchange(
        ptr::null_mut(),
        candidate.cast(),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {
            // SAFETY: the candidate is now installed in the mutex slot.
            unsafe { &*candidate }
        }
        Err(installed) => {
            // SAFETY: the losing candidate was never published.
            unsafe { drop(Box::from_raw(candidate)) };
            // SAFETY: the winning pointer remains installed until mutex drop.
            unsafe { &*installed.cast::<HostWaitQueue>() }
        }
    }
}

#[cfg(feature = "sleep")]
fn wait_until_unlocked(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64) {
    let queue = ensure_wait_queue(wait_queue);
    let mut state = queue.state.lock().expect("host wait queue poisoned");
    queue.waiters.fetch_add(1, Ordering::AcqRel);
    while owner_id.load(Ordering::Acquire) != 0 {
        state = queue
            .condvar
            .wait(state)
            .expect("host wait queue poisoned while waiting");
    }
    queue.waiters.fetch_sub(1, Ordering::AcqRel);
}

#[cfg(feature = "sleep")]
fn wake_one(wait_queue: &AtomicPtr<()>) {
    let queue = wait_queue.load(Ordering::Acquire).cast::<HostWaitQueue>();
    if queue.is_null() {
        return;
    }
    // SAFETY: installed queue pointers remain valid until exclusive mutex drop.
    let queue = unsafe { &*queue };
    let _state = queue.state.lock().expect("host wait queue poisoned");
    queue.condvar.notify_one();
}

#[cfg(feature = "sleep")]
struct PendingMutexAcquire<'a> {
    owner_id: &'a AtomicU64,
    wait_queue: &'a AtomicPtr<()>,
    acquired: bool,
}

#[cfg(feature = "sleep")]
impl Drop for PendingMutexAcquire<'_> {
    fn drop(&mut self) {
        if self.acquired {
            self.owner_id.store(0, Ordering::Release);
            wake_one(self.wait_queue);
        }
    }
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_acquire(
    wait_queue: &AtomicPtr<()>,
    owner_id: &AtomicU64,
    metadata: &LockMetadata,
    lock_addr: usize,
    subclass: u32,
    is_try: bool,
    caller: &'static Location<'static>,
) -> bool {
    if !is_try {
        MIGHT_SLEEP_CALLS.set(MIGHT_SLEEP_CALLS.get() + 1);
        assert_eq!(
            host_preempt_depth(),
            0,
            "sleeping mutex acquired with preemption disabled at {caller}"
        );
    }
    let current_id = current_task_id();
    let lockdep = PendingLockdepAcquire::prepare(LockRequest {
        metadata,
        addr: lock_addr,
        mode: LOCK_MODE_EXCLUSIVE,
        subclass,
        sleep_forbidden: false,
        caller,
    });
    let acquired = if is_try {
        owner_id
            .compare_exchange(0, current_id, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    } else {
        loop {
            match owner_id.compare_exchange_weak(
                0,
                current_id,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => break true,
                Err(owner) => {
                    assert_ne!(
                        owner, current_id,
                        "task {current_id} tried to recursively acquire a mutex"
                    );
                    wait_until_unlocked(wait_queue, owner_id);
                }
            }
        }
    };
    let mut pending = PendingMutexAcquire {
        owner_id,
        wait_queue,
        acquired,
    };
    lockdep.finish(acquired);
    pending.acquired = false;
    acquired
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_release(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, lock_addr: usize) {
    let owner = owner_id.load(Ordering::Acquire);
    let current = current_task_id();
    assert_eq!(
        owner, current,
        "task {current} tried to release a mutex owned by task {owner}"
    );
    release_lockdep(lock_addr);
    owner_id.store(0, Ordering::Release);
    wake_one(wait_queue);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_force_release(
    wait_queue: &AtomicPtr<()>,
    owner_id: &AtomicU64,
    lock_addr: usize,
) {
    mutex_release(wait_queue, owner_id, lock_addr);
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_is_owned_by_current(owner_id: &AtomicU64) -> bool {
    owner_id.load(Ordering::Acquire) == current_task_id()
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_is_locked(owner_id: &AtomicU64) -> bool {
    owner_id.load(Ordering::Acquire) != 0
}

#[cfg(feature = "sleep")]
pub(crate) fn mutex_drop_wait_queue(wait_queue: *mut ()) {
    // SAFETY: the bridge passes a pointer removed by exclusive mutex drop.
    let queue = unsafe { Box::from_raw(wait_queue.cast::<HostWaitQueue>()) };
    assert_eq!(
        queue.waiters.load(Ordering::Acquire),
        0,
        "dropping a host wait queue with active waiters"
    );
}

pub(crate) fn set_trace_enabled(enabled: bool) {
    TRACE_ENABLED.store(enabled, Ordering::Release);
}

pub(crate) fn dump_trace() {
    let _ = TRACE_ENABLED.load(Ordering::Acquire);
}

#[cfg(all(test, feature = "sleep"))]
pub(crate) fn might_sleep_calls() -> usize {
    MIGHT_SLEEP_CALLS.get()
}

#[cfg(all(test, feature = "sleep"))]
pub(crate) fn reset_might_sleep_calls() {
    MIGHT_SLEEP_CALLS.set(0);
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, Barrier},
        thread,
    };

    use super::host_context_snapshot;
    #[cfg(feature = "sleep")]
    use super::{might_sleep_calls, reset_might_sleep_calls};
    #[cfg(feature = "sleep")]
    use crate::Mutex;
    use crate::{SpinLock, SpinRwLock};

    #[test]
    fn acquisition_modes_restore_host_context() {
        let lock = SpinLock::new(());
        assert_eq!(host_context_snapshot(), (0, true));
        let guard = lock.lock();
        assert_eq!(host_context_snapshot(), (1, true));
        drop(guard);
        assert_eq!(host_context_snapshot(), (0, true));

        let guard = lock.lock_irqsave();
        assert_eq!(host_context_snapshot(), (1, false));
        drop(guard);
        assert_eq!(host_context_snapshot(), (0, true));
    }

    #[test]
    fn failed_try_lock_restores_context() {
        let lock = Arc::new(SpinLock::new(()));
        let barrier = Arc::new(Barrier::new(2));
        let held_lock = lock.clone();
        let held_barrier = barrier.clone();
        let holder = thread::spawn(move || {
            let _guard = held_lock.lock();
            held_barrier.wait();
            held_barrier.wait();
        });
        barrier.wait();
        assert!(lock.try_lock_irqsave().is_none());
        assert_eq!(host_context_snapshot(), (0, true));
        barrier.wait();
        holder.join().expect("spin holder panicked");
    }

    #[test]
    fn spin_lock_is_mutually_exclusive_and_publishes_writes() {
        const THREADS: usize = 8;
        const ITERATIONS: usize = 2_000;
        let value = Arc::new(SpinLock::new((0usize, 0usize)));
        let mut workers = Vec::new();
        for _ in 0..THREADS {
            let value = value.clone();
            workers.push(thread::spawn(move || {
                for _ in 0..ITERATIONS {
                    let mut guard = value.lock();
                    guard.0 += 1;
                    guard.1 = guard.0;
                }
            }));
        }
        for worker in workers {
            worker.join().expect("spin worker panicked");
        }
        let guard = value.lock();
        assert_eq!(guard.0, THREADS * ITERATIONS);
        assert_eq!(guard.1, guard.0);
    }

    #[test]
    fn raw_and_rwlock_modes_preserve_their_contracts() {
        let raw = SpinLock::new(1usize);
        let mut guard = unsafe { raw.lock_raw() };
        *guard += 1;
        assert_eq!(host_context_snapshot(), (0, true));
        drop(guard);

        let rw = SpinRwLock::new(2usize);
        let first = rw.read();
        let second = rw.try_read().expect("concurrent reader should enter");
        assert_eq!((*first, *second), (2, 2));
        drop((first, second));
        *rw.write() = 3;
        assert_eq!(*rw.read(), 3);
    }

    #[test]
    fn lockdep_panics_roll_back_lock_and_context_state() {
        let lock = SpinLock::new(());
        let guard = lock.lock();
        let recursive = catch_unwind(AssertUnwindSafe(|| lock.lock()));
        assert!(recursive.is_err());
        assert_eq!(host_context_snapshot(), (1, true));
        drop(guard);
        assert_eq!(host_context_snapshot(), (0, true));
        assert!(lock.try_lock().is_some());

        let first = SpinLock::new(());
        let second = SpinLock::new(());
        {
            let _first = first.lock();
            let _second = second.lock();
        }
        let second_guard = second.lock();
        let inverted = catch_unwind(AssertUnwindSafe(|| first.lock()));
        assert!(inverted.is_err());
        assert_eq!(host_context_snapshot(), (1, true));
        drop(second_guard);
        assert_eq!(host_context_snapshot(), (0, true));
    }

    #[cfg(feature = "sleep")]
    #[test]
    fn mutex_lock_rejects_preemption_disabled_context() {
        let spin = SpinLock::new(());
        let mutex = Mutex::new(());
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _spin_guard = spin.lock();
            let _mutex_guard = mutex.lock();
        }));
        assert!(result.is_err());
    }

    #[cfg(feature = "sleep")]
    #[test]
    fn mutex_waits_wakes_and_releases_one_owner_at_a_time() {
        let mutex = Arc::new(Mutex::new(0usize));
        let barrier = Arc::new(Barrier::new(3));
        let guard = mutex.lock();
        let mut waiters = Vec::new();
        for value in 1..=2 {
            let mutex = mutex.clone();
            let barrier = barrier.clone();
            waiters.push(thread::spawn(move || {
                barrier.wait();
                *mutex.lock() += value;
            }));
        }
        barrier.wait();
        drop(guard);
        for waiter in waiters {
            waiter.join().expect("mutex waiter panicked");
        }
        assert_eq!(*mutex.lock(), 3);
        // SAFETY: this diagnostic borrow does not mutate ownership state.
        assert!(unsafe { mutex.raw() }.host_wait_queue_installed());
    }

    #[cfg(feature = "sleep")]
    #[test]
    fn mutex_try_lock_does_not_sleep_or_allocate() {
        reset_might_sleep_calls();
        let mutex = Arc::new(Mutex::new(()));
        let barrier = Arc::new(Barrier::new(2));
        let held_mutex = mutex.clone();
        let held_barrier = barrier.clone();
        let holder = thread::spawn(move || {
            let _guard = held_mutex.lock();
            held_barrier.wait();
            held_barrier.wait();
        });
        barrier.wait();
        assert!(mutex.try_lock().is_none());
        assert_eq!(might_sleep_calls(), 0);
        // SAFETY: this diagnostic borrow does not mutate ownership state.
        assert!(!unsafe { mutex.raw() }.host_wait_queue_installed());
        barrier.wait();
        holder.join().expect("mutex holder panicked");
    }

    #[cfg(feature = "sleep")]
    #[test]
    fn mutex_rejects_wrong_owner_and_supports_force_unlock() {
        let mutex = Arc::new(Mutex::new(()));
        let guard = mutex.lock();
        core::mem::forget(guard);
        let wrong_owner = mutex.clone();
        let result = thread::spawn(move || {
            catch_unwind(AssertUnwindSafe(|| unsafe {
                wrong_owner.force_unlock();
            }))
        })
        .join()
        .expect("wrong-owner worker panicked outside catch_unwind");
        assert!(result.is_err());
        // SAFETY: the current thread owns exactly the forgotten guard above.
        unsafe { mutex.force_unlock() };
        assert!(!mutex.is_locked());
    }
}
