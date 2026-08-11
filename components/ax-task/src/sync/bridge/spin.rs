//! Complete external spin and read-write lock transactions.

use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize},
};

use super::context::{ContextOperations, ContextState, context_enter, context_exit};
use crate::sync::spin::atomic;
#[cfg(feature = "lockdep")]
use crate::sync::{
    context::IrqSaveGuard,
    lockdep::LockdepMapView,
    spin::lockdep::{self, LockdepAcquireRequest},
};

const LOCK_MODE_READ: u8 = 1;
const LOCK_MODE_WRITE: u8 = 2;

/// Borrowed lock-class storage from an external fixed-layout wrapper.
#[derive(Clone, Copy)]
pub struct LockClass<'lock> {
    pub class_id: &'lock AtomicU32,
    pub class_key: &'lock AtomicPtr<Location<'static>>,
}

/// One complete external exclusive-spin acquisition request.
pub struct SpinAcquireRequest<'lock> {
    pub locked: &'lock AtomicBool,
    pub class: LockClass<'lock>,
    pub lock_addr: usize,
    pub context: u8,
    pub subclass: u32,
    pub is_try: bool,
    pub caller: &'static Location<'static>,
}

/// One complete external spin read-write acquisition request.
pub struct RwLockAcquireRequest<'lock> {
    pub state: &'lock AtomicUsize,
    pub class: LockClass<'lock>,
    pub lock_addr: usize,
    pub context: u8,
    pub mode: u8,
    pub is_try: bool,
    pub caller: &'static Location<'static>,
}

/// Acquires an external exclusive spin lock through the native state machine.
pub fn spin_acquire(
    request: SpinAcquireRequest<'_>,
    operations: &ContextOperations,
) -> (bool, ContextState) {
    let pending_context = PendingContext::enter(request.context, operations);
    let lockdep = prepare_spin_lockdep(&request);
    let acquired = acquire_spin_state(request.locked, request.is_try, lockdep);

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
) -> (bool, ContextState) {
    let pending_context = PendingContext::enter(request.context, operations);
    let lockdep = prepare_rwlock_lockdep(&request);
    let acquired = acquire_rwlock_state(request.state, request.mode, request.is_try);
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

fn prepare_spin_lockdep(request: &SpinAcquireRequest<'_>) -> LockdepAcquire {
    #[cfg(feature = "lockdep")]
    {
        prepare_lockdep(BridgeLockdepRequest {
            class: request.class,
            lock_kind: "spin lock",
            trace_kind: "spin",
            lock_addr: request.lock_addr,
            context: request.context,
            subclass: request.subclass,
            is_try: request.is_try,
            caller: request.caller,
            track_task_lock: true,
        })
    }

    #[cfg(not(feature = "lockdep"))]
    {
        let _ = request;
        LockdepAcquire
    }
}

fn prepare_rwlock_lockdep(request: &RwLockAcquireRequest<'_>) -> LockdepAcquire {
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
            is_try: request.is_try,
            caller: request.caller,
            track_task_lock,
        })
    }

    #[cfg(not(feature = "lockdep"))]
    {
        let _ = (request, track_task_lock);
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

fn acquire_spin_state(locked: &AtomicBool, is_try: bool, lockdep: LockdepAcquire) -> bool {
    #[cfg(feature = "smp")]
    {
        if is_try {
            let acquired = spin_try_acquire_with_lockdep(locked, lockdep);
            if !acquired {
                finish_spin_try_failure(lockdep);
            }
            acquired
        } else {
            atomic::spin_acquire(locked, || {
                spin_acquire_once_weak_with_lockdep(locked, lockdep)
            });
            true
        }
    }

    #[cfg(not(feature = "smp"))]
    {
        let _ = (locked, is_try);
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

fn acquire_rwlock_state(state: &AtomicUsize, mode: u8, is_try: bool) -> bool {
    match (mode, is_try) {
        (LOCK_MODE_READ, true) => atomic::rw_try_acquire_read(state),
        (LOCK_MODE_READ, false) => {
            atomic::rw_acquire_read(state);
            true
        }
        (LOCK_MODE_WRITE, true) => atomic::rw_try_acquire_write(state),
        (LOCK_MODE_WRITE, false) => {
            atomic::rw_acquire_write(state);
            true
        }
        (mode, _) => panic!("unknown external rwlock mode {mode}"),
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

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::sync::bridge::context::{CONTEXT_PREEMPT, CONTEXT_RAW};

    static PREEMPT_ENTERS: AtomicUsize = AtomicUsize::new(0);
    static PREEMPT_EXITS: AtomicUsize = AtomicUsize::new(0);
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn preempt_enter() -> usize {
        PREEMPT_ENTERS.fetch_add(1, Ordering::Relaxed);
        0x55
    }

    unsafe fn preempt_exit(state: usize) {
        assert_eq!(state, 0x55);
        PREEMPT_EXITS.fetch_add(1, Ordering::Relaxed);
    }

    unsafe fn preempt_exit_irq_return(_state: usize) {}
    fn irq_save_and_disable() -> usize {
        0
    }
    unsafe fn irq_restore(_state: usize) {}
    fn no_op() {}

    fn operations() -> ContextOperations {
        ContextOperations {
            preempt_enter,
            preempt_exit,
            preempt_exit_irq_return,
            irq_save_and_disable,
            irq_restore,
            hardirq_enter: no_op,
            hardirq_exit: no_op,
        }
    }

    fn class() -> (AtomicU32, AtomicPtr<Location<'static>>) {
        (AtomicU32::new(0), AtomicPtr::new(core::ptr::null_mut()))
    }

    fn reset_context_counts() {
        PREEMPT_ENTERS.store(0, Ordering::Relaxed);
        PREEMPT_EXITS.store(0, Ordering::Relaxed);
    }

    #[cfg(feature = "smp")]
    #[test]
    fn failed_spin_try_restores_context_without_changing_lock_word() {
        let _serial = TEST_LOCK.lock().unwrap();
        #[cfg(feature = "lockdep")]
        let _runtime = crate::test_runtime::InstalledDefaultTaskRuntime::new();
        reset_context_counts();
        let locked = AtomicBool::new(true);
        let (class_id, class_key) = class();
        let (acquired, state) = spin_acquire(
            SpinAcquireRequest {
                locked: &locked,
                class: LockClass {
                    class_id: &class_id,
                    class_key: &class_key,
                },
                lock_addr: core::ptr::from_ref(&locked) as usize,
                context: CONTEXT_PREEMPT,
                subclass: 0,
                is_try: true,
                caller: Location::caller(),
            },
            &operations(),
        );

        assert!(!acquired);
        assert_eq!(state, ContextState::new(0, 0));
        assert!(locked.load(Ordering::Relaxed));
        assert_eq!(PREEMPT_ENTERS.load(Ordering::Relaxed), 1);
        assert_eq!(PREEMPT_EXITS.load(Ordering::Relaxed), 1);
    }

    #[cfg(feature = "smp")]
    #[test]
    fn raw_spin_success_uses_native_state_and_release_algorithm() {
        let _serial = TEST_LOCK.lock().unwrap();
        #[cfg(feature = "lockdep")]
        let _runtime = crate::test_runtime::InstalledDefaultTaskRuntime::new();
        let locked = AtomicBool::new(false);
        let (class_id, class_key) = class();
        let lock_addr = core::ptr::from_ref(&locked) as usize;
        let (acquired, state) = spin_acquire(
            SpinAcquireRequest {
                locked: &locked,
                class: LockClass {
                    class_id: &class_id,
                    class_key: &class_key,
                },
                lock_addr,
                context: CONTEXT_RAW,
                subclass: 0,
                is_try: false,
                caller: Location::caller(),
            },
            &operations(),
        );

        assert!(acquired);
        assert!(spin_is_locked(&locked));
        spin_release(&locked, lock_addr, CONTEXT_RAW, state, &operations());
        assert!(!spin_is_locked(&locked));
    }

    #[test]
    fn rw_writer_blocks_reader_try_and_failed_context_is_restored() {
        let _serial = TEST_LOCK.lock().unwrap();
        #[cfg(feature = "lockdep")]
        let _runtime = crate::test_runtime::InstalledDefaultTaskRuntime::new();
        reset_context_counts();
        let state_word = AtomicUsize::new(0);
        let (writer_class_id, writer_class_key) = class();
        let lock_addr = core::ptr::from_ref(&state_word) as usize;
        let (writer_acquired, writer_context) = rwlock_acquire(
            RwLockAcquireRequest {
                state: &state_word,
                class: LockClass {
                    class_id: &writer_class_id,
                    class_key: &writer_class_key,
                },
                lock_addr,
                context: CONTEXT_RAW,
                mode: LOCK_MODE_WRITE,
                is_try: false,
                caller: Location::caller(),
            },
            &operations(),
        );
        assert!(writer_acquired);

        let (reader_class_id, reader_class_key) = class();
        let (reader_acquired, reader_context) = rwlock_acquire(
            RwLockAcquireRequest {
                state: &state_word,
                class: LockClass {
                    class_id: &reader_class_id,
                    class_key: &reader_class_key,
                },
                lock_addr: lock_addr + 1,
                context: CONTEXT_PREEMPT,
                mode: LOCK_MODE_READ,
                is_try: true,
                caller: Location::caller(),
            },
            &operations(),
        );
        assert!(!reader_acquired);
        assert_eq!(reader_context, ContextState::new(0, 0));
        assert_eq!(PREEMPT_ENTERS.load(Ordering::Relaxed), 1);
        assert_eq!(PREEMPT_EXITS.load(Ordering::Relaxed), 1);

        rwlock_release(
            &state_word,
            lock_addr,
            CONTEXT_RAW,
            writer_context,
            LOCK_MODE_WRITE,
            &operations(),
        );
        assert!(atomic::rw_try_acquire_read(&state_word));
        atomic::rw_release_read(&state_word);
    }

    #[cfg(feature = "lockdep")]
    #[test]
    fn external_spin_and_rw_write_share_native_task_held_state() {
        let _serial = TEST_LOCK.lock().unwrap();
        let _runtime = crate::test_runtime::InstalledDefaultTaskRuntime::new();

        let locked = AtomicBool::new(false);
        let (spin_class_id, spin_class_key) = class();
        let spin_addr = core::ptr::from_ref(&locked) as usize;
        let (acquired, spin_context) = spin_acquire(
            SpinAcquireRequest {
                locked: &locked,
                class: LockClass {
                    class_id: &spin_class_id,
                    class_key: &spin_class_key,
                },
                lock_addr: spin_addr,
                context: CONTEXT_RAW,
                subclass: 0,
                is_try: false,
                caller: Location::caller(),
            },
            &operations(),
        );
        assert!(acquired);
        assert!(crate::sync::lockdep::current_task_held_lock_snapshot().contains_addr(spin_addr));
        spin_release(&locked, spin_addr, CONTEXT_RAW, spin_context, &operations());
        assert!(!crate::sync::lockdep::current_task_held_lock_snapshot().contains_addr(spin_addr));

        let rw_state = AtomicUsize::new(0);
        let (rw_class_id, rw_class_key) = class();
        let rw_addr = core::ptr::from_ref(&rw_state) as usize;
        let (read_acquired, read_context) = rwlock_acquire(
            RwLockAcquireRequest {
                state: &rw_state,
                class: LockClass {
                    class_id: &rw_class_id,
                    class_key: &rw_class_key,
                },
                lock_addr: rw_addr,
                context: CONTEXT_RAW,
                mode: LOCK_MODE_READ,
                is_try: false,
                caller: Location::caller(),
            },
            &operations(),
        );
        assert!(read_acquired);
        assert!(!crate::sync::lockdep::current_task_held_lock_snapshot().contains_addr(rw_addr));
        rwlock_release(
            &rw_state,
            rw_addr,
            CONTEXT_RAW,
            read_context,
            LOCK_MODE_READ,
            &operations(),
        );

        let (write_acquired, write_context) = rwlock_acquire(
            RwLockAcquireRequest {
                state: &rw_state,
                class: LockClass {
                    class_id: &rw_class_id,
                    class_key: &rw_class_key,
                },
                lock_addr: rw_addr,
                context: CONTEXT_RAW,
                mode: LOCK_MODE_WRITE,
                is_try: false,
                caller: Location::caller(),
            },
            &operations(),
        );
        assert!(write_acquired);
        assert!(crate::sync::lockdep::current_task_held_lock_snapshot().contains_addr(rw_addr));
        rwlock_release(
            &rw_state,
            rw_addr,
            CONTEXT_RAW,
            write_context,
            LOCK_MODE_WRITE,
            &operations(),
        );
        assert!(!crate::sync::lockdep::current_task_held_lock_snapshot().contains_addr(rw_addr));
    }
}
