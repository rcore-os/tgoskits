//! Complete external spin and read-write lock transactions.

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize},
};

use super::{
    context::{ContextOperations, ContextState, context_enter, context_exit},
    lockdep::LockClass,
};
use crate::sync::spin::atomic;
#[cfg(feature = "lockdep")]
use crate::sync::{
    context::IrqSaveGuard,
    lockdep::LockdepMapView,
    spin::lockdep::{self, LockdepAcquireRequest},
};

const LOCK_MODE_READ: u8 = 1;
const LOCK_MODE_WRITE: u8 = 2;

/// One complete external exclusive-spin acquisition request.
pub struct SpinAcquireRequest<'lock> {
    pub locked: &'lock AtomicBool,
    pub class: LockClass<'lock>,
    pub lock_addr: usize,
    pub context: u8,
    pub subclass: u32,
    pub caller: &'static Location<'static>,
}

/// One complete external spin read-write acquisition request.
pub struct RwLockAcquireRequest<'lock> {
    pub state: &'lock AtomicUsize,
    pub class: LockClass<'lock>,
    pub lock_addr: usize,
    pub context: u8,
    pub mode: u8,
    pub caller: &'static Location<'static>,
}

/// Acquires an external exclusive spin lock through the native state machine.
pub fn spin_acquire(
    request: SpinAcquireRequest<'_>,
    operations: &ContextOperations,
) -> ContextState {
    let pending_context = PendingContext::enter(request.context, operations);
    let lockdep = prepare_spin_lockdep(&request, AcquireKind::Blocking);
    acquire_spin_state(request.locked, lockdep);
    pending_context.into_state()
}

/// Attempts an external exclusive spin acquisition through the native state machine.
pub fn spin_try_acquire(
    request: SpinAcquireRequest<'_>,
    operations: &ContextOperations,
) -> (bool, ContextState) {
    let pending_context = PendingContext::enter(request.context, operations);
    let lockdep = prepare_spin_lockdep(&request, AcquireKind::Try);
    let acquired = try_acquire_spin_state(request.locked, lockdep);
    if acquired {
        (true, pending_context.into_state())
    } else {
        drop(pending_context);
        (false, ContextState::new(0, 0))
    }
}

/// Releases an external exclusive spin lock and its execution context.
pub fn spin_release(
    locked: &AtomicBool,
    lock_addr: usize,
    context: u8,
    context_state: ContextState,
    operations: &ContextOperations,
) {
    release_spin_state(locked, lock_addr, context);
    context_exit(context, context_state, operations);
}

/// Releases a deliberately leaked external exclusive spin acquisition.
pub fn spin_force_release(locked: &AtomicBool, lock_addr: usize, context: u8) {
    force_release_spin_state(locked, lock_addr, context);
}

/// Returns a diagnostic snapshot of an external spin lock word.
pub fn spin_is_locked(locked: &AtomicBool) -> bool {
    #[cfg(feature = "smp")]
    {
        atomic::spin_is_locked(locked)
    }

    #[cfg(not(feature = "smp"))]
    {
        let _ = locked;
        false
    }
}

/// Acquires an external spin read-write lock through the native state machine.
pub fn rwlock_acquire(
    request: RwLockAcquireRequest<'_>,
    operations: &ContextOperations,
) -> ContextState {
    let pending_context = PendingContext::enter(request.context, operations);
    let lockdep = prepare_rwlock_lockdep(&request, AcquireKind::Blocking);
    acquire_rwlock_state(request.state, request.mode);
    finish_rwlock_lockdep(lockdep, true);
    pending_context.into_state()
}

/// Attempts an external spin read-write acquisition through the native state machine.
pub fn rwlock_try_acquire(
    request: RwLockAcquireRequest<'_>,
    operations: &ContextOperations,
) -> (bool, ContextState) {
    let pending_context = PendingContext::enter(request.context, operations);
    let lockdep = prepare_rwlock_lockdep(&request, AcquireKind::Try);
    let acquired = try_acquire_rwlock_state(request.state, request.mode);
    finish_rwlock_lockdep(lockdep, acquired);
    if acquired {
        (true, pending_context.into_state())
    } else {
        drop(pending_context);
        (false, ContextState::new(0, 0))
    }
}

/// Releases an external spin read-write acquisition and its context.
pub fn rwlock_release(
    state: &AtomicUsize,
    lock_addr: usize,
    context: u8,
    context_state: ContextState,
    mode: u8,
    operations: &ContextOperations,
) {
    release_rwlock_state(state, lock_addr, context, mode);
    context_exit(context, context_state, operations);
}

/// Removes one deliberately leaked external raw read acquisition.
pub fn rwlock_force_read_decrement(state: &AtomicUsize, lock_addr: usize, context: u8) {
    if atomic::rw_force_read_decrement(state) {
        release_rwlock_read_lockdep(lock_addr, context);
    }
}

struct PendingContext<'operations> {
    context: u8,
    state: Option<ContextState>,
    operations: &'operations ContextOperations,
}

impl<'operations> PendingContext<'operations> {
    fn enter(context: u8, operations: &'operations ContextOperations) -> Self {
        Self {
            context,
            state: Some(context_enter(context, operations)),
            operations,
        }
    }

    fn into_state(mut self) -> ContextState {
        self.state
            .take()
            .expect("pending external context state must be owned")
    }
}

impl Drop for PendingContext<'_> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            context_exit(self.context, state, self.operations);
        }
    }
}

#[cfg(feature = "lockdep")]
type LockdepAcquire = lockdep::Lockdep;

#[cfg(not(feature = "lockdep"))]
#[derive(Clone, Copy)]
struct LockdepAcquire;

#[derive(Clone, Copy)]
enum AcquireKind {
    Blocking,
    Try,
}

#[cfg(feature = "lockdep")]
struct BridgeLockdepRequest<'lock> {
    class: LockClass<'lock>,
    lock_kind: &'static str,
    trace_kind: &'static str,
    lock_addr: usize,
    context: u8,
    subclass: u32,
    is_try: bool,
    caller: &'static Location<'static>,
    track_task_lock: bool,
}

fn prepare_spin_lockdep(
    request: &SpinAcquireRequest<'_>,
    acquire_kind: AcquireKind,
) -> LockdepAcquire {
    #[cfg(feature = "lockdep")]
    {
        prepare_lockdep(BridgeLockdepRequest {
            class: request.class,
            lock_kind: "spin lock",
            trace_kind: "spin",
            lock_addr: request.lock_addr,
            context: request.context,
            subclass: request.subclass,
            is_try: matches!(acquire_kind, AcquireKind::Try),
            caller: request.caller,
            track_task_lock: true,
        })
    }

    #[cfg(not(feature = "lockdep"))]
    {
        let _ = (request, acquire_kind);
        LockdepAcquire
    }
}

fn prepare_rwlock_lockdep(
    request: &RwLockAcquireRequest<'_>,
    acquire_kind: AcquireKind,
) -> LockdepAcquire {
    let track_task_lock = match request.mode {
        LOCK_MODE_READ => false,
        LOCK_MODE_WRITE => true,
        mode => panic!("unknown external rwlock mode {mode}"),
    };
    #[cfg(feature = "lockdep")]
    {
        prepare_lockdep(BridgeLockdepRequest {
            class: request.class,
            lock_kind: "spin rwlock",
            trace_kind: "spin-rwlock",
            lock_addr: request.lock_addr,
            context: request.context,
            subclass: 0,
            is_try: matches!(acquire_kind, AcquireKind::Try),
            caller: request.caller,
            track_task_lock,
        })
    }

    #[cfg(not(feature = "lockdep"))]
    {
        let _ = (request, acquire_kind, track_task_lock);
        LockdepAcquire
    }
}

#[cfg(feature = "lockdep")]
fn prepare_lockdep(request: BridgeLockdepRequest<'_>) -> LockdepAcquire {
    LockdepAcquire::prepare_view(LockdepAcquireRequest {
        map: LockdepMapView::new(request.class.class_id, request.class.class_key),
        lock_kind: request.lock_kind,
        trace_kind: request.trace_kind,
        addr: request.lock_addr,
        is_try: request.is_try,
        subclass: request.subclass,
        caller: request.caller,
        detail: context_detail(request.context),
        track_task_lock: request.track_task_lock && context_tracks_task_locks(request.context),
    })
}

fn acquire_spin_state(locked: &AtomicBool, lockdep: LockdepAcquire) {
    #[cfg(feature = "smp")]
    {
        atomic::spin_acquire(locked, || {
            spin_acquire_once_weak_with_lockdep(locked, lockdep)
        });
    }

    #[cfg(not(feature = "smp"))]
    {
        let _ = locked;
        finish_lockdep_with_irqsave(lockdep, true);
    }
}

fn try_acquire_spin_state(locked: &AtomicBool, lockdep: LockdepAcquire) -> bool {
    #[cfg(feature = "smp")]
    {
        let acquired = spin_try_acquire_with_lockdep(locked, lockdep);
        if !acquired {
            finish_spin_try_failure(lockdep);
        }
        acquired
    }

    #[cfg(not(feature = "smp"))]
    {
        let _ = locked;
        finish_lockdep_with_irqsave(lockdep, true);
        true
    }
}

#[cfg(feature = "smp")]
fn spin_acquire_once_weak_with_lockdep(locked: &AtomicBool, lockdep: LockdepAcquire) -> bool {
    with_lockdep_irqsave(|| {
        let acquired = atomic::spin_try_acquire_weak(locked);
        if acquired {
            finish_lockdep(lockdep, true);
        }
        acquired
    })
}

#[cfg(feature = "smp")]
fn spin_try_acquire_with_lockdep(locked: &AtomicBool, lockdep: LockdepAcquire) -> bool {
    with_lockdep_irqsave(|| {
        let acquired = atomic::spin_try_acquire_strong(locked);
        if acquired {
            finish_lockdep(lockdep, true);
        }
        acquired
    })
}

fn release_spin_state(locked: &AtomicBool, lock_addr: usize, context: u8) {
    with_lockdep_irqsave(|| {
        release_spin_lockdep(lock_addr, context, false);
        #[cfg(feature = "smp")]
        atomic::spin_release(locked);
        #[cfg(not(feature = "smp"))]
        let _ = locked;
    });
}

fn force_release_spin_state(locked: &AtomicBool, lock_addr: usize, context: u8) {
    with_lockdep_irqsave(|| {
        release_spin_lockdep(lock_addr, context, true);
        #[cfg(feature = "smp")]
        atomic::spin_release(locked);
        #[cfg(not(feature = "smp"))]
        let _ = locked;
    });
}

fn acquire_rwlock_state(state: &AtomicUsize, mode: u8) {
    match mode {
        LOCK_MODE_READ => {
            atomic::rw_acquire_read(state);
        }
        LOCK_MODE_WRITE => {
            atomic::rw_acquire_write(state);
        }
        mode => panic!("unknown external rwlock mode {mode}"),
    }
}

fn try_acquire_rwlock_state(state: &AtomicUsize, mode: u8) -> bool {
    match mode {
        LOCK_MODE_READ => atomic::rw_try_acquire_read(state),
        LOCK_MODE_WRITE => atomic::rw_try_acquire_write(state),
        mode => panic!("unknown external rwlock mode {mode}"),
    }
}

fn release_rwlock_state(state: &AtomicUsize, lock_addr: usize, context: u8, mode: u8) {
    with_lockdep_irqsave(|| match mode {
        LOCK_MODE_READ => {
            release_rwlock_read_lockdep(lock_addr, context);
            atomic::rw_release_read(state);
        }
        LOCK_MODE_WRITE => {
            release_rwlock_write_lockdep(lock_addr, context);
            atomic::rw_release_write(state);
        }
        mode => panic!("unknown external rwlock mode {mode}"),
    });
}

fn finish_rwlock_lockdep(lockdep: LockdepAcquire, acquired: bool) {
    finish_lockdep_with_irqsave(lockdep, acquired);
}

fn finish_lockdep_with_irqsave(lockdep: LockdepAcquire, acquired: bool) {
    with_lockdep_irqsave(|| finish_lockdep(lockdep, acquired));
}

fn finish_lockdep(lockdep: LockdepAcquire, acquired: bool) {
    #[cfg(feature = "lockdep")]
    lockdep.finish(acquired);

    #[cfg(not(feature = "lockdep"))]
    let _ = (lockdep, acquired);
}

#[cfg(feature = "smp")]
fn finish_spin_try_failure(lockdep: LockdepAcquire) {
    finish_lockdep(lockdep, false);
}

fn release_spin_lockdep(lock_addr: usize, context: u8, force: bool) {
    #[cfg(feature = "lockdep")]
    if force {
        lockdep::force_release_external(
            "spin",
            lock_addr,
            context_detail(context),
            context_tracks_task_locks(context),
        );
    } else {
        lockdep::release_external(
            "spin",
            lock_addr,
            context_detail(context),
            context_tracks_task_locks(context),
        );
    }

    #[cfg(not(feature = "lockdep"))]
    let _ = (lock_addr, context, force);
}

fn release_rwlock_read_lockdep(lock_addr: usize, context: u8) {
    #[cfg(feature = "lockdep")]
    lockdep::release_external("spin-rwlock", lock_addr, context_detail(context), false);

    #[cfg(not(feature = "lockdep"))]
    let _ = (lock_addr, context);
}

fn release_rwlock_write_lockdep(lock_addr: usize, context: u8) {
    #[cfg(feature = "lockdep")]
    lockdep::release_external(
        "spin-rwlock",
        lock_addr,
        context_detail(context),
        context_tracks_task_locks(context),
    );

    #[cfg(not(feature = "lockdep"))]
    let _ = (lock_addr, context);
}

fn with_lockdep_irqsave<R>(operation: impl FnOnce() -> R) -> R {
    #[cfg(feature = "lockdep")]
    let _irq_guard = IrqSaveGuard::new();
    operation()
}

#[cfg(feature = "lockdep")]
fn context_tracks_task_locks(context: u8) -> bool {
    match context {
        super::context::CONTEXT_RAW
        | super::context::CONTEXT_PREEMPT
        | super::context::CONTEXT_PREEMPT_IRQSAVE => true,
        super::context::CONTEXT_IRQSAVE => false,
        context => panic!("unknown external lock context {context}"),
    }
}

#[cfg(feature = "lockdep")]
fn context_detail(context: u8) -> &'static str {
    match context {
        super::context::CONTEXT_RAW => "external raw context",
        super::context::CONTEXT_PREEMPT => "external preempt context",
        super::context::CONTEXT_IRQSAVE => "external irq-save context",
        super::context::CONTEXT_PREEMPT_IRQSAVE => "external preempt+irq-save context",
        context => panic!("unknown external lock context {context}"),
    }
}
