//! Primitive operations used by the `ax-runtime` adapter for `ax-sync`.
//!
//! This module is public only because the provider lives in another crate.
//! OS consumers must use [`crate::sync`] or `ax-runtime::sync` instead.

#[cfg(feature = "multitask")]
use core::sync::atomic::AtomicU64;
use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering},
};

use super::{GuardState, IrqSaveState, PreemptIrqSaveState, PreemptState};

const CONTEXT_RAW: u8 = 0;
const CONTEXT_PREEMPT: u8 = 1;
const CONTEXT_IRQSAVE: u8 = 2;
const CONTEXT_PREEMPT_IRQSAVE: u8 = 3;

const LOCK_MODE_EXCLUSIVE: u8 = 0;
const LOCK_MODE_READ: u8 = 1;
const LOCK_MODE_WRITE: u8 = 2;

const READER: usize = 1;
const WRITER: usize = 1 << (usize::BITS - 1);
const MAX_READER: usize = 1 << (usize::BITS - 2);

/// Lock-class metadata borrowed from an `ax-sync` bridge object.
pub struct LockClass<'a> {
    pub class_id: &'a AtomicU32,
    pub class_key: &'a AtomicPtr<Location<'static>>,
}

/// Complete spin-lock acquisition request from the runtime provider.
pub struct SpinAcquireRequest<'a> {
    pub locked: &'a AtomicBool,
    pub class: LockClass<'a>,
    pub lock_addr: usize,
    pub context: u8,
    pub subclass: u32,
    pub is_try: bool,
    pub caller: &'static Location<'static>,
}

/// Complete spin read-write lock acquisition request from the runtime provider.
pub struct RwLockAcquireRequest<'a> {
    pub state: &'a AtomicUsize,
    pub class: LockClass<'a>,
    pub lock_addr: usize,
    pub context: u8,
    pub mode: u8,
    pub is_try: bool,
    pub caller: &'static Location<'static>,
}

/// Complete sleepable-mutex acquisition request from the runtime provider.
#[cfg(feature = "multitask")]
pub struct MutexAcquireRequest<'a> {
    pub wait_queue: &'a AtomicPtr<()>,
    pub owner_id: &'a AtomicU64,
    pub class: LockClass<'a>,
    pub lock_addr: usize,
    pub subclass: u32,
    pub is_try: bool,
    pub caller: &'static Location<'static>,
}

struct LockdepAcquireRequest<'a> {
    class: LockClass<'a>,
    lock_kind: &'static str,
    trace_kind: &'static str,
    addr: usize,
    context: u8,
    mode: u8,
    subclass: u32,
    is_try: bool,
    sleep_forbidden: bool,
    caller: &'static Location<'static>,
}

/// Enters one execution-context mode and returns its restore token.
pub fn context_enter(context: u8) -> usize {
    match context {
        CONTEXT_RAW => 0,
        CONTEXT_PREEMPT => PreemptState::acquire(),
        CONTEXT_IRQSAVE => IrqSaveState::acquire(),
        CONTEXT_PREEMPT_IRQSAVE => PreemptIrqSaveState::acquire(),
        _ => panic!("unknown lock context mode {context}"),
    }
}

/// Leaves one execution-context mode using its matching restore token.
pub fn context_exit(context: u8, state: usize) {
    match context {
        CONTEXT_RAW => {}
        CONTEXT_PREEMPT => PreemptState::release(state),
        CONTEXT_IRQSAVE => IrqSaveState::release(state),
        CONTEXT_PREEMPT_IRQSAVE => PreemptIrqSaveState::release(state),
        _ => panic!("unknown lock context mode {context}"),
    }
}

/// Returns host-test preemption depth and IRQ-enabled state.
#[cfg(all(feature = "host-test", not(target_os = "none")))]
pub fn host_context_snapshot() -> (usize, bool) {
    super::context::host_context_snapshot()
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

#[cfg(feature = "lockdep")]
struct BridgeLockdepAcquire {
    addr: usize,
    inner: super::lockdep::Lockdep,
    prepared: Option<super::lockdep::PreparedAcquire>,
}

#[cfg(feature = "lockdep")]
impl BridgeLockdepAcquire {
    fn prepare(request: LockdepAcquireRequest<'_>) -> Self {
        // SAFETY: `ax-sync::LockMetadata` documents the same repr(C) prefix.
        let map = unsafe {
            super::lockdep::LockdepMap::from_external_parts(
                request.class.class_id,
                request.class.class_key,
            )
        };
        let mode = held_mode(request.mode);
        let track_task =
            mode != super::lockdep::HeldLockMode::Read || request.context != CONTEXT_RAW;
        let prepared = track_task.then(|| {
            let snapshot = super::lockdep::current_task_held_lock_snapshot();
            if request.sleep_forbidden {
                super::lockdep::prepare_acquire_with_snapshot_nested_mode(
                    map,
                    request.lock_kind,
                    request.addr,
                    request.caller,
                    snapshot,
                    request.subclass,
                    mode,
                )
            } else {
                super::lockdep::prepare_acquire_with_snapshot_nested_with_sleep(
                    map,
                    request.lock_kind,
                    request.addr,
                    request.caller,
                    snapshot,
                    request.subclass,
                    false,
                )
            }
        });
        Self {
            addr: request.addr,
            inner: super::lockdep::Lockdep::prepare(
                request.trace_kind,
                request.addr,
                request.is_try,
                None,
            ),
            prepared,
        }
    }

    fn finish(&self, acquired: bool) {
        let _irq_guard = super::IrqSaveGuard::new();
        self.inner.finish(acquired);
        if let (true, Some(prepared)) = (acquired, self.prepared) {
            super::lockdep::finish_acquire_task(prepared, self.addr);
        }
    }
}

#[cfg(not(feature = "lockdep"))]
struct BridgeLockdepAcquire;

#[cfg(not(feature = "lockdep"))]
impl BridgeLockdepAcquire {
    fn prepare(request: LockdepAcquireRequest<'_>) -> Self {
        let LockdepAcquireRequest {
            class,
            lock_kind,
            trace_kind,
            addr,
            context,
            mode,
            subclass,
            is_try,
            sleep_forbidden,
            caller,
        } = request;
        let _ = (
            class,
            lock_kind,
            trace_kind,
            addr,
            context,
            mode,
            subclass,
            is_try,
            sleep_forbidden,
            caller,
        );
        Self
    }

    fn finish(&self, _acquired: bool) {}
}

#[cfg(feature = "lockdep")]
fn held_mode(mode: u8) -> super::lockdep::HeldLockMode {
    match mode {
        LOCK_MODE_EXCLUSIVE => super::lockdep::HeldLockMode::Exclusive,
        LOCK_MODE_READ => super::lockdep::HeldLockMode::Read,
        LOCK_MODE_WRITE => super::lockdep::HeldLockMode::Write,
        _ => panic!("unknown lock ownership mode {mode}"),
    }
}

#[cfg(feature = "lockdep")]
fn lockdep_release(trace_kind: &'static str, addr: usize, context: u8, mode: u8) {
    let mode = held_mode(mode);
    let track_task = mode != super::lockdep::HeldLockMode::Read || context != CONTEXT_RAW;
    let _irq_guard = super::IrqSaveGuard::new();
    if track_task {
        super::lockdep::release_task(addr);
    }
    super::lockdep::Lockdep::release(trace_kind, addr, None);
}

#[cfg(not(feature = "lockdep"))]
fn lockdep_release(_trace_kind: &'static str, _addr: usize, _context: u8, _mode: u8) {}

#[cfg(feature = "lockdep")]
fn lockdep_force_release(trace_kind: &'static str, addr: usize, context: u8, mode: u8) {
    let mode = held_mode(mode);
    let track_task = mode != super::lockdep::HeldLockMode::Read || context != CONTEXT_RAW;
    let _irq_guard = super::IrqSaveGuard::new();
    if track_task {
        super::lockdep::force_release_task(addr);
    }
    super::lockdep::Lockdep::release(trace_kind, addr, None);
}

#[cfg(not(feature = "lockdep"))]
fn lockdep_force_release(_trace_kind: &'static str, _addr: usize, _context: u8, _mode: u8) {}

struct PendingSpinAcquire<'a> {
    context: Option<PendingContext>,
    #[cfg(feature = "smp")]
    locked: &'a AtomicBool,
    #[cfg(not(feature = "smp"))]
    _locked: core::marker::PhantomData<&'a AtomicBool>,
    acquired: bool,
}

impl<'a> PendingSpinAcquire<'a> {
    fn new(locked: &'a AtomicBool, context: u8) -> Self {
        #[cfg(not(feature = "smp"))]
        let _ = locked;
        Self {
            context: Some(PendingContext::enter(context)),
            #[cfg(feature = "smp")]
            locked,
            #[cfg(not(feature = "smp"))]
            _locked: core::marker::PhantomData,
            acquired: false,
        }
    }

    fn finish(mut self) -> usize {
        self.acquired = false;
        self.context.take().expect("missing lock context").disarm()
    }
}

impl Drop for PendingSpinAcquire<'_> {
    fn drop(&mut self) {
        if self.acquired {
            #[cfg(feature = "smp")]
            self.locked.store(false, Ordering::Release);
        }
    }
}

/// Performs a complete spin acquisition transaction.
pub fn spin_acquire(request: SpinAcquireRequest<'_>) -> (bool, usize) {
    let mut pending = PendingSpinAcquire::new(request.locked, request.context);
    let lockdep = BridgeLockdepAcquire::prepare(LockdepAcquireRequest {
        class: request.class,
        lock_kind: "spin lock",
        trace_kind: "spin",
        addr: request.lock_addr,
        context: request.context,
        mode: LOCK_MODE_EXCLUSIVE,
        subclass: request.subclass,
        is_try: request.is_try,
        sleep_forbidden: true,
        caller: request.caller,
    });

    #[cfg(feature = "smp")]
    let acquired = if request.is_try {
        request
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    } else {
        loop {
            if request
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break true;
            }
            while request.locked.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
    };
    #[cfg(not(feature = "smp"))]
    let acquired = true;

    pending.acquired = acquired;
    lockdep.finish(acquired);
    if acquired {
        (true, pending.finish())
    } else {
        (false, 0)
    }
}

/// Releases a spin acquisition and restores its execution context.
pub fn spin_release(locked: &AtomicBool, lock_addr: usize, context: u8, context_state: usize) {
    lockdep_release("spin", lock_addr, context, LOCK_MODE_EXCLUSIVE);
    #[cfg(feature = "smp")]
    locked.store(false, Ordering::Release);
    #[cfg(not(feature = "smp"))]
    let _ = locked;
    context_exit(context, context_state);
}

/// Releases a deliberately leaked spin acquisition without restoring context.
pub fn spin_force_release(locked: &AtomicBool, lock_addr: usize, context: u8) {
    lockdep_force_release("spin", lock_addr, context, LOCK_MODE_EXCLUSIVE);
    #[cfg(feature = "smp")]
    locked.store(false, Ordering::Release);
    #[cfg(not(feature = "smp"))]
    let _ = locked;
}

/// Returns a diagnostic spin-lock snapshot.
pub fn spin_is_locked(locked: &AtomicBool) -> bool {
    #[cfg(feature = "smp")]
    {
        locked.load(Ordering::Acquire)
    }
    #[cfg(not(feature = "smp"))]
    {
        let _ = locked;
        false
    }
}

struct PendingRwAcquire<'a> {
    context: Option<PendingContext>,
    state: &'a AtomicUsize,
    mode: Option<u8>,
}

impl<'a> PendingRwAcquire<'a> {
    fn new(state: &'a AtomicUsize, context: u8) -> Self {
        Self {
            context: Some(PendingContext::enter(context)),
            state,
            mode: None,
        }
    }

    fn finish(mut self) -> usize {
        self.mode = None;
        self.context
            .take()
            .expect("missing rwlock context")
            .disarm()
    }
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

/// Performs a complete read or write acquisition transaction.
pub fn rwlock_acquire(request: RwLockAcquireRequest<'_>) -> (bool, usize) {
    let mut pending = PendingRwAcquire::new(request.state, request.context);
    let lockdep = BridgeLockdepAcquire::prepare(LockdepAcquireRequest {
        class: request.class,
        lock_kind: "spin rwlock",
        trace_kind: "spin-rwlock",
        addr: request.lock_addr,
        context: request.context,
        mode: request.mode,
        subclass: 0,
        is_try: request.is_try,
        sleep_forbidden: true,
        caller: request.caller,
    });
    let acquire_once = || match request.mode {
        LOCK_MODE_READ => try_read(request.state),
        LOCK_MODE_WRITE => try_write(request.state),
        mode => panic!("unknown rwlock acquisition mode {mode}"),
    };
    let acquired = if request.is_try {
        acquire_once()
    } else {
        loop {
            if acquire_once() {
                break true;
            }
            match request.mode {
                LOCK_MODE_READ => {
                    while request.state.load(Ordering::Acquire) & WRITER != 0 {
                        core::hint::spin_loop();
                    }
                }
                LOCK_MODE_WRITE => {
                    while request.state.load(Ordering::Acquire) != 0 {
                        core::hint::spin_loop();
                    }
                }
                _ => unreachable!(),
            }
        }
    };
    if acquired {
        pending.mode = Some(request.mode);
    }
    lockdep.finish(acquired);
    if acquired {
        (true, pending.finish())
    } else {
        (false, 0)
    }
}

/// Releases a read or write acquisition and restores its context.
pub fn rwlock_release(
    state: &AtomicUsize,
    lock_addr: usize,
    context: u8,
    context_state: usize,
    mode: u8,
) {
    lockdep_release("spin-rwlock", lock_addr, context, mode);
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

/// Removes one leaked raw read acquisition.
pub fn rwlock_force_read_decrement(state: &AtomicUsize, lock_addr: usize, _context: u8) {
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
                #[cfg(feature = "lockdep")]
                {
                    let _irq_guard = super::IrqSaveGuard::new();
                    super::lockdep::Lockdep::release("spin-rwlock", lock_addr, None);
                }
                #[cfg(not(feature = "lockdep"))]
                let _ = lock_addr;
                return;
            }
            Err(current) => observed = current,
        }
    }
}

#[cfg(feature = "multitask")]
fn current_task_id() -> u64 {
    let id = super::mutex::runtime_current_task_id();
    assert_ne!(id, 0, "task runtime returned reserved owner id 0");
    id
}

#[cfg(feature = "multitask")]
struct PendingMutexAcquire<'a> {
    owner_id: &'a AtomicU64,
    wait_queue: &'a AtomicPtr<()>,
    acquired: bool,
}

#[cfg(feature = "multitask")]
impl Drop for PendingMutexAcquire<'_> {
    fn drop(&mut self) {
        if self.acquired {
            self.owner_id.store(0, Ordering::Release);
            super::mutex::runtime_wake_one(self.wait_queue);
        }
    }
}

/// Performs a complete sleepable mutex acquisition transaction.
#[cfg(feature = "multitask")]
pub fn mutex_acquire(request: MutexAcquireRequest<'_>) -> bool {
    if !request.is_try {
        super::mutex::runtime_might_sleep(request.caller);
    }
    let current_id = current_task_id();
    let lockdep = BridgeLockdepAcquire::prepare(LockdepAcquireRequest {
        class: request.class,
        lock_kind: "mutex",
        trace_kind: "mutex",
        addr: request.lock_addr,
        context: CONTEXT_PREEMPT,
        mode: LOCK_MODE_EXCLUSIVE,
        subclass: request.subclass,
        is_try: request.is_try,
        sleep_forbidden: false,
        caller: request.caller,
    });
    let mut pending = PendingMutexAcquire {
        owner_id: request.owner_id,
        wait_queue: request.wait_queue,
        acquired: false,
    };
    let acquired = if request.is_try {
        request
            .owner_id
            .compare_exchange(0, current_id, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    } else {
        loop {
            match request.owner_id.compare_exchange_weak(
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
                    super::mutex::runtime_wait_until_unlocked(request.wait_queue, request.owner_id);
                }
            }
        }
    };
    pending.acquired = acquired;
    lockdep.finish(acquired);
    pending.acquired = false;
    acquired
}

/// Releases a sleepable mutex after validating task ownership.
#[cfg(feature = "multitask")]
pub fn mutex_release(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, lock_addr: usize) {
    let owner = owner_id.load(Ordering::Acquire);
    let current = current_task_id();
    assert_eq!(
        owner, current,
        "task {current} tried to release a mutex owned by task {owner}"
    );
    lockdep_release("mutex", lock_addr, CONTEXT_PREEMPT, LOCK_MODE_EXCLUSIVE);
    owner_id.store(0, Ordering::Release);
    super::mutex::runtime_wake_one(wait_queue);
}

/// Releases a deliberately leaked sleepable mutex guard.
#[cfg(feature = "multitask")]
pub fn mutex_force_release(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, lock_addr: usize) {
    mutex_release(wait_queue, owner_id, lock_addr);
}

/// Returns whether the current task owns a sleepable mutex.
#[cfg(feature = "multitask")]
pub fn mutex_is_owned_by_current(owner_id: &AtomicU64) -> bool {
    owner_id.load(Ordering::Acquire) == current_task_id()
}

/// Returns whether a sleepable mutex has an owner.
#[cfg(feature = "multitask")]
pub fn mutex_is_locked(owner_id: &AtomicU64) -> bool {
    owner_id.load(Ordering::Acquire) != 0
}

/// Validates and destroys an opaque mutex wait queue.
#[cfg(feature = "multitask")]
pub fn mutex_drop_wait_queue(wait_queue: *mut ()) {
    super::mutex::runtime_drop_wait_queue(wait_queue);
}

/// Enables or disables the native lockdep trace.
pub fn set_lockdep_trace_enabled(enabled: bool) {
    super::set_lockdep_trace_enabled(enabled);
}

/// Dumps the native lockdep trace.
pub fn dump_lockdep_trace() {
    super::dump_lockdep_trace();
}
