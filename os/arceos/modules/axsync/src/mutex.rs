//! Priority-inheritance sleeping mutex.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::{
    PiMutexAcquire, PiMutexClaimOutcome, PiMutexCore, PiMutexLockResult, PiMutexOwnedRelease,
    PiMutexStateError, PiTaskId, PiWaitCancelOutcome, PiWaitToken, PreemptGuard,
};

/// A non-recursive, urgency-ordered PI mutex implementing `lock_api::RawMutex`.
///
/// The uncontended path uses a Linux rtmutex-style atomic owner word. Its high
/// bit forces contenders through the metadata lock while waiter publication,
/// donation registration, and handoff are in progress. Blocking and targeted
/// wake happen after that metadata guard has been released.
pub struct RawMutex {
    core: PiMutexCore,
    next_waiter_sequence: AtomicU64,
    #[cfg(feature = "lockdep")]
    pub(crate) lockdep: crate::spin_lockdep::LockdepMap,
}

enum LockAttempt {
    Acquired,
    Contended,
}

/// Interruption observed while waiting for a PI mutex.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PiMutexLockInterrupted;

impl core::fmt::Display for PiMutexLockInterrupted {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("PI mutex wait interrupted")
    }
}

impl core::error::Error for PiMutexLockInterrupted {}

/// Linux rtmutex-style interruptible acquisition for a PI mutex.
pub trait InterruptibleMutexExt<T: ?Sized> {
    /// Acquires this mutex unless `should_interrupt` becomes true while the
    /// caller remains queued.
    ///
    /// A published ownerless handoff wins over interruption. The returned
    /// guard therefore has the same acquire-before-signal ordering as Linux
    /// `mutex_lock_interruptible()` under PREEMPT_RT.
    fn lock_interruptible<F>(
        &self,
        should_interrupt: F,
    ) -> Result<MutexGuard<'_, T>, PiMutexLockInterrupted>
    where
        F: FnMut() -> bool;
}

const OWNER_SPIN_BATCH: usize = 64;

#[cfg(not(feature = "lockdep"))]
/// A lockdep subclass identifier when lockdep is disabled.
pub type LockSubclass = u32;
#[cfg(feature = "lockdep")]
pub type LockSubclass = crate::spin_lockdep::LockSubclass;

/// Adds lockdep subclass acquisition to a sleeping [`Mutex`].
pub trait LockdepMutexExt<T: ?Sized> {
    /// Acquires the mutex using `subclass` for lock-order validation.
    fn lock_nested(&self, subclass: LockSubclass) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> LockdepMutexExt<T> for Mutex<T> {
    #[inline(always)]
    #[track_caller]
    fn lock_nested(&self, subclass: LockSubclass) -> MutexGuard<'_, T> {
        #[cfg(not(feature = "lockdep"))]
        {
            let _ = subclass;
            self.lock()
        }

        #[cfg(feature = "lockdep")]
        {
            // SAFETY: the raw reference is used for the matching acquisition.
            let raw = unsafe { self.raw() };
            raw.lock_nested(subclass);
            // SAFETY: `lock_nested` acquired this mutex.
            unsafe { self.make_guard_unchecked() }
        }
    }
}

impl RawMutex {
    /// Creates an unlocked PI mutex.
    pub const fn new() -> Self {
        Self {
            core: PiMutexCore::new(),
            next_waiter_sequence: AtomicU64::new(0),
            #[cfg(feature = "lockdep")]
            lockdep: crate::spin_lockdep::LockdepMap::new(),
        }
    }

    /// Returns whether the current thread owns this mutex.
    pub fn is_owned_by_current(&self) -> bool {
        self.core.is_owned_by(Self::current_task_id())
    }

    #[inline(always)]
    #[cfg(all(test, target_os = "none"))]
    fn mutex_ref(&self) -> crate::PiMutexRef<'_> {
        core_result(self.core.mutex_ref(), "borrow PI mutex core")
    }

    #[inline(always)]
    fn current_task_id() -> PiTaskId {
        let task_id =
            ax_crate_interface::call_interface!(crate::pi::PiMutexTaskOps::current_task_id);
        PiTaskId::new(task_id).expect("PI mutex runtime returned an invalid task identity")
    }

    fn lock_pi(&self) {
        let mut blocking_context_validated = false;

        loop {
            let (current, attempt) = self.try_or_observe_current();
            match attempt {
                LockAttempt::Acquired => return,
                LockAttempt::Contended => {
                    if !blocking_context_validated {
                        // The uncontended path neither publishes a waiter nor
                        // schedules and must remain usable during
                        // single-threaded boot.
                        ax_crate_interface::call_interface!(
                            crate::pi::PiMutexTaskOps::validate_blocking_context
                        );
                        blocking_context_validated = true;
                        continue;
                    }
                    self.lock_contended(current);
                    return;
                }
            }
        }
    }

    fn lock_pi_interruptible(
        &self,
        mut should_interrupt: impl FnMut() -> bool,
    ) -> Result<(), PiMutexLockInterrupted> {
        let mut blocking_context_validated = false;

        loop {
            let (current, attempt) = self.try_or_observe_current();
            match attempt {
                LockAttempt::Acquired => return Ok(()),
                LockAttempt::Contended => {
                    if !blocking_context_validated {
                        ax_crate_interface::call_interface!(
                            crate::pi::PiMutexTaskOps::validate_blocking_context
                        );
                        blocking_context_validated = true;
                        continue;
                    }
                    return self.lock_contended_interruptible(current, &mut should_interrupt);
                }
            }
        }
    }

    fn lock_contended(&self, current: PiTaskId) {
        let sequence = self.next_waiter_sequence.fetch_add(1, Ordering::Relaxed);
        let token = match ax_crate_interface::call_interface!(
            crate::pi::PiMutexTaskOps::lock_slow,
            &self.core,
            sequence
        ) {
            PiMutexLockResult::Acquired => return,
            PiMutexLockResult::Waiting(token) => token,
        };
        debug_assert_eq!(token.thread_id(), current);
        if self.try_claim_waiter(&token) {
            return;
        }
        self.wait_for_handoff(token);
    }

    fn lock_contended_interruptible(
        &self,
        current: PiTaskId,
        should_interrupt: &mut impl FnMut() -> bool,
    ) -> Result<(), PiMutexLockInterrupted> {
        let sequence = self.next_waiter_sequence.fetch_add(1, Ordering::Relaxed);
        let token = match ax_crate_interface::call_interface!(
            crate::pi::PiMutexTaskOps::lock_slow,
            &self.core,
            sequence
        ) {
            PiMutexLockResult::Acquired => return Ok(()),
            PiMutexLockResult::Waiting(token) => token,
        };
        debug_assert_eq!(token.thread_id(), current);

        loop {
            if token.is_granted() || self.try_claim_waiter(&token) {
                return Ok(());
            }
            if should_interrupt() {
                match ax_crate_interface::call_interface!(
                    crate::pi::PiMutexTaskOps::try_cancel,
                    &token
                ) {
                    PiWaitCancelOutcome::Cancelled => return Err(PiMutexLockInterrupted),
                    PiWaitCancelOutcome::HandoffPending => continue,
                }
            }
            if !token.can_claim() && !self.spin_on_owner(&token) {
                ax_crate_interface::call_interface!(
                    crate::pi::PiMutexTaskOps::park_current_once,
                    &token
                );
            }
        }
    }

    fn wait_for_handoff(&self, token: PiWaitToken) {
        loop {
            if token.is_granted() {
                break;
            }
            if self.try_claim_waiter(&token) {
                break;
            }
            if !token.can_claim() && !self.spin_on_owner(&token) {
                ax_crate_interface::call_interface!(
                    crate::pi::PiMutexTaskOps::park_current_once,
                    &token
                );
            }
        }
        assert!(
            self.core.is_owned_by(token.thread_id()),
            "PI core owner must name the granted waiter"
        );
    }

    /// Spins only while the registered waiter can make progress under the same
    /// gates as Linux `rtmutex_spin_on_owner`: the observed owner is unchanged
    /// and executing, this waiter remains most urgent, and the current CPU has
    /// no pending reschedule request.
    fn spin_on_owner(&self, token: &PiWaitToken) -> bool {
        let _preempt_guard = PreemptGuard::new();
        let Some(owner) = token.initial_owner() else {
            return token.can_claim() || token.is_granted();
        };

        loop {
            if token.can_claim() || token.is_granted() {
                return true;
            }

            if !self.core.is_owned_by(owner) {
                return token.can_claim() || token.is_granted();
            }
            let owner_on_cpu = token.initial_owner_is_on_cpu();
            // SAFETY: `_preempt_guard` pins this caller to one CPU throughout
            // the observation and the following spin batch.
            let need_resched = ax_crate_interface::call_interface!(
                crate::pi::PiMutexTaskOps::current_needs_reschedule_pinned
            );
            let waiter_is_top = token.is_top_waiter();

            if !owner_spin_eligible(
                self.core.is_owned_by(owner),
                owner_on_cpu,
                waiter_is_top,
                need_resched,
            ) {
                return token.can_claim() || token.is_granted();
            }

            for _ in 0..OWNER_SPIN_BATCH {
                if token.can_claim() || token.is_granted() {
                    return true;
                }
                if !self.core.is_owned_by(owner) {
                    return token.can_claim() || token.is_granted();
                }
                core::hint::spin_loop();
            }
        }
    }

    #[cfg(all(test, target_os = "none"))]
    fn try_or_observe_owner(&self, current: PiTaskId) -> LockAttempt {
        match core_result(
            self.core.try_acquire(current),
            "try modeled PI mutex acquisition",
        ) {
            PiMutexAcquire::Acquired => LockAttempt::Acquired,
            PiMutexAcquire::Contended => LockAttempt::Contended,
        }
    }

    fn try_or_observe_current(&self) -> (PiTaskId, LockAttempt) {
        let current = Self::current_task_id();
        let attempt = self.try_or_observe_current_token(current);
        (current, attempt)
    }

    fn try_or_observe_current_token(&self, current: PiTaskId) -> LockAttempt {
        match core_result(self.core.try_acquire(current), "try PI mutex acquisition") {
            PiMutexAcquire::Acquired => LockAttempt::Acquired,
            PiMutexAcquire::Contended => LockAttempt::Contended,
        }
    }

    fn try_lock_pi(&self) -> bool {
        let current = Self::current_task_id();
        match self.core.try_acquire(current) {
            Ok(PiMutexAcquire::Acquired) => true,
            Ok(PiMutexAcquire::Contended) | Err(PiMutexStateError::WaiterOwnsLock) => false,
            Err(error) => panic!("try PI mutex failed: {error}"),
        }
    }

    fn try_claim_waiter(&self, token: &PiWaitToken) -> bool {
        if token.is_granted() {
            return true;
        }
        if !token.can_claim() {
            return false;
        }
        match ax_crate_interface::call_interface!(crate::pi::PiMutexTaskOps::claim, token) {
            PiMutexClaimOutcome::Claimed => true,
            PiMutexClaimOutcome::Retry => false,
        }
    }

    unsafe fn unlock_pi(&self) {
        // SAFETY: the caller is the lock_api raw-mutex owner and retains that
        // exclusive authority through this complete release transaction.
        match core_result(
            unsafe { self.core.try_release_owned() },
            "try PI mutex release",
        ) {
            PiMutexOwnedRelease::Released => {}
            PiMutexOwnedRelease::Contended(owner) => {
                // SAFETY: `owner` came from this core's owner-authorized release
                // result and the raw-mutex contract remains active.
                unsafe { self.unlock_contended(owner) };
            }
        }
    }

    unsafe fn unlock_contended(&self, owner: PiTaskId) {
        ax_crate_interface::call_interface!(
            crate::pi::PiMutexTaskOps::release_owned,
            &self.core,
            owner
        );
    }

    #[cfg(feature = "lockdep")]
    #[track_caller]
    fn lock_nested(&self, subclass: LockSubclass) {
        let lockdep = crate::mutex_lockdep::LockdepAcquire::prepare_nested(self, false, subclass);
        self.lock_pi();
        lockdep.finish(true);
    }

    #[cfg(feature = "lockdep")]
    #[track_caller]
    fn lock_interruptible_nested(
        &self,
        subclass: LockSubclass,
        should_interrupt: impl FnMut() -> bool,
    ) -> Result<(), PiMutexLockInterrupted> {
        let lockdep = crate::mutex_lockdep::LockdepAcquire::prepare_nested(self, false, subclass);
        let result = self.lock_pi_interruptible(should_interrupt);
        lockdep.finish(result.is_ok());
        result
    }

    #[cfg(feature = "lockdep")]
    #[track_caller]
    fn try_lock_nested(&self, subclass: LockSubclass) -> bool {
        let lockdep = crate::mutex_lockdep::LockdepAcquire::prepare_nested(self, true, subclass);
        let acquired = self.try_lock_pi();
        lockdep.finish(acquired);
        acquired
    }
}

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: task-context metadata transitions are serialized by a preemption-safe
// gate. Hard IRQ paths never access this state. A lock_api guard is created only
// after scheduler ownership registration or an explicit PI handoff grants the
// calling thread.
unsafe impl lock_api::RawMutex for RawMutex {
    type GuardMarker = lock_api::GuardNoSend;

    const INIT: Self = Self::new();

    #[inline(always)]
    #[track_caller]
    fn lock(&self) {
        #[cfg(feature = "lockdep")]
        self.lock_nested(crate::lockdep_core::DEFAULT_LOCK_SUBCLASS);

        #[cfg(not(feature = "lockdep"))]
        self.lock_pi();
    }

    #[inline(always)]
    #[track_caller]
    fn try_lock(&self) -> bool {
        #[cfg(feature = "lockdep")]
        {
            self.try_lock_nested(crate::lockdep_core::DEFAULT_LOCK_SUBCLASS)
        }

        #[cfg(not(feature = "lockdep"))]
        {
            self.try_lock_pi()
        }
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        #[cfg(feature = "lockdep")]
        crate::mutex_lockdep::release(self);
        // SAFETY: lock_api calls `unlock` only for the execution context that
        // owns this raw mutex, and this method consumes that ownership once.
        unsafe { self.unlock_pi() };
    }

    #[inline(always)]
    fn is_locked(&self) -> bool {
        self.core.is_locked()
    }
}

fn owner_spin_eligible(
    same_owner: bool,
    owner_on_cpu: bool,
    waiter_is_top: bool,
    need_resched: bool,
) -> bool {
    same_owner && owner_on_cpu && waiter_is_top && !need_resched
}

#[track_caller]
fn core_result<T>(result: Result<T, PiMutexStateError>, operation: &'static str) -> T {
    result.unwrap_or_else(|error| panic!("{operation} failed: {error}"))
}

/// A safe PI mutex using [`RawMutex`].
pub type Mutex<T> = lock_api::Mutex<RawMutex, T>;
/// A non-send guard returned by [`Mutex`].
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawMutex, T>;

impl<T: ?Sized> InterruptibleMutexExt<T> for Mutex<T> {
    #[track_caller]
    fn lock_interruptible<F>(
        &self,
        should_interrupt: F,
    ) -> Result<MutexGuard<'_, T>, PiMutexLockInterrupted>
    where
        F: FnMut() -> bool,
    {
        // SAFETY: this reference is used only for the matching acquisition;
        // the returned guard retains the safe mutex borrow.
        let raw = unsafe { self.raw() };
        #[cfg(feature = "lockdep")]
        raw.lock_interruptible_nested(
            crate::lockdep_core::DEFAULT_LOCK_SUBCLASS,
            should_interrupt,
        )?;
        #[cfg(not(feature = "lockdep"))]
        raw.lock_pi_interruptible(should_interrupt)?;

        // SAFETY: the raw acquisition above established current as owner.
        Ok(unsafe { self.make_guard_unchecked() })
    }
}

#[cfg(all(feature = "host-test", not(target_os = "none")))]
mod host {
    use core::{cell::Cell, sync::atomic::Ordering};
    use std::{
        boxed::Box,
        collections::VecDeque,
        sync::{Condvar, Mutex as StdMutex},
    };

    use crate::{
        PiMutexClaimOutcome, PiMutexCore, PiMutexLockResult, PiMutexTaskOps, PiTaskId,
        PiWaitCancelOutcome, PiWaitToken,
    };

    struct HostWaitQueue {
        waiters: StdMutex<VecDeque<(PiTaskId, u64)>>,
        changed: Condvar,
    }

    struct HostWaitHandle {
        queue: Box<HostWaitQueue>,
    }

    impl HostWaitQueue {
        fn new() -> Self {
            Self {
                waiters: StdMutex::new(VecDeque::new()),
                changed: Condvar::new(),
            }
        }
    }

    static NEXT_TASK_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

    std::thread_local! {
        static TASK_ID: Cell<u64> = const { Cell::new(0) };
    }

    struct HostPiMutexTaskOps;

    #[ax_crate_interface::impl_interface]
    impl PiMutexTaskOps for HostPiMutexTaskOps {
        fn current_task_id() -> u64 {
            host_task_id().get()
        }

        fn validate_blocking_context() {}

        fn lock_slow(lock: &PiMutexCore, sequence: u64) -> PiMutexLockResult {
            let current = host_task_id();
            let queue = ensure_wait_queue(lock);
            let mut waiters = queue.waiters.lock().expect("host PI wait queue poisoned");
            loop {
                let snapshot = lock.owner_snapshot();
                if snapshot.is_unlocked() {
                    assert!(
                        waiters.is_empty(),
                        "unlocked host PI mutex retained waiters"
                    );
                    if lock.try_acquire_snapshot(snapshot, current) {
                        return PiMutexLockResult::Acquired;
                    }
                    continue;
                }
                assert_ne!(
                    snapshot.owner(),
                    Some(current),
                    "host task recursively acquired a PI mutex"
                );
                if !lock.try_mark_waiters(snapshot) {
                    continue;
                }
                let generation = sequence
                    .checked_add(1)
                    .expect("host PI waiter generation overflow");
                waiters.push_back((current, generation));
                let lock_ref = lock.mutex_ref().expect("host PI mutex identity exhausted");
                return PiMutexLockResult::Waiting(unsafe {
                    // SAFETY: the queue entry above retains this exact lock,
                    // task, and generation until claim or cancellation.
                    PiWaitToken::from_registration(
                        lock_ref.raw(),
                        current,
                        snapshot.owner(),
                        generation,
                        core::ptr::NonNull::dangling(),
                    )
                });
            }
        }

        fn waiter_is_granted(token: &PiWaitToken) -> bool {
            unsafe { token.lock_raw().core() }.is_owned_by(token.thread_id())
        }

        fn waiter_is_top(token: &PiWaitToken) -> bool {
            let queue = token_queue(token);
            queue
                .waiters
                .lock()
                .expect("host PI wait queue poisoned")
                .front()
                .is_some_and(|entry| *entry == token_entry(token))
        }

        fn initial_owner_is_on_cpu(token: &PiWaitToken) -> bool {
            token.initial_owner().is_some()
        }

        fn current_needs_reschedule_pinned() -> bool {
            false
        }

        fn park_current_once(token: &PiWaitToken) {
            let queue = token_queue(token);
            let core = unsafe { token.lock_raw().core() };
            let mut waiters = queue.waiters.lock().expect("host PI wait queue poisoned");
            while !core.is_owned_by(token.thread_id())
                && !(core.owner_snapshot().is_ownerless()
                    && waiters
                        .front()
                        .is_some_and(|entry| *entry == token_entry(token)))
            {
                waiters = queue
                    .changed
                    .wait(waiters)
                    .expect("host PI wait queue poisoned while parked");
            }
        }

        fn try_cancel(token: &PiWaitToken) -> PiWaitCancelOutcome {
            let queue = token_queue(token);
            let core = unsafe { token.lock_raw().core() };
            let mut waiters = queue.waiters.lock().expect("host PI wait queue poisoned");
            if core.owner_snapshot().is_ownerless()
                && waiters
                    .front()
                    .is_some_and(|entry| *entry == token_entry(token))
            {
                return PiWaitCancelOutcome::HandoffPending;
            }
            let position = waiters
                .iter()
                .position(|entry| *entry == token_entry(token))
                .expect("host PI cancellation lost its waiter");
            waiters.remove(position);
            if waiters.is_empty() {
                let snapshot = core.owner_snapshot();
                if let Some(owner) = snapshot.owner() {
                    core.clear_waiters_bit(owner);
                } else if snapshot.is_ownerless() {
                    core.publish_unlocked();
                }
            }
            PiWaitCancelOutcome::Cancelled
        }

        fn claim(token: &PiWaitToken) -> PiMutexClaimOutcome {
            let queue = token_queue(token);
            let core = unsafe { token.lock_raw().core() };
            let mut waiters = queue.waiters.lock().expect("host PI wait queue poisoned");
            if !core.owner_snapshot().is_ownerless()
                || waiters
                    .front()
                    .is_none_or(|entry| *entry != token_entry(token))
            {
                return PiMutexClaimOutcome::Retry;
            }
            waiters.pop_front();
            core.publish_owner(token.thread_id(), !waiters.is_empty());
            queue.changed.notify_all();
            PiMutexClaimOutcome::Claimed
        }

        fn release_owned(lock: &PiMutexCore, old_owner: PiTaskId) {
            let queue = installed_wait_queue(lock);
            let waiters = queue.waiters.lock().expect("host PI wait queue poisoned");
            let snapshot = lock.owner_snapshot();
            assert_eq!(snapshot.owner(), Some(old_owner));
            assert!(snapshot.has_waiters());
            assert!(!waiters.is_empty());
            lock.publish_ownerless();
            queue.changed.notify_all();
        }

        fn drop_wait_handle(wait_handle: *mut ()) {
            let handle = wait_handle.cast::<HostWaitHandle>();
            // SAFETY: PiMutexCore transfers the unique initialized inline
            // handle after mutable destruction excludes every safe waiter.
            let handle = unsafe { handle.read() };
            assert!(
                handle
                    .queue
                    .waiters
                    .lock()
                    .expect("host PI wait queue poisoned")
                    .is_empty(),
                "dropping a host PI mutex with active waiters"
            );
        }
    }

    fn host_task_id() -> PiTaskId {
        let raw = TASK_ID.with(|task_id| match task_id.get() {
            0 => {
                let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
                task_id.set(id);
                id
            }
            id => id,
        });
        PiTaskId::new(raw).expect("host PI task identity overflow")
    }

    fn ensure_wait_queue(lock: &PiMutexCore) -> &HostWaitQueue {
        let handle = unsafe {
            // SAFETY: the host provider is the sole interpreter of this
            // storage and always installs `HostWaitHandle`.
            lock.wait_storage().get_or_init(|| HostWaitHandle {
                queue: Box::new(HostWaitQueue::new()),
            })
        };
        &handle.queue
    }

    fn installed_wait_queue(lock: &PiMutexCore) -> &HostWaitQueue {
        let handle = unsafe {
            // SAFETY: the host provider installs only `HostWaitHandle`.
            lock.wait_storage()
                .get::<HostWaitHandle>()
                .expect("contended host PI mutex has no wait queue")
        };
        &handle.queue
    }

    fn token_queue(token: &PiWaitToken) -> &HostWaitQueue {
        installed_wait_queue(unsafe { token.lock_raw().core() })
    }

    fn token_entry(token: &PiWaitToken) -> (PiTaskId, u64) {
        (token.thread_id(), token.generation())
    }
}

#[cfg(all(test, feature = "host-test", not(target_os = "none")))]
mod host_tests {
    use std::{sync::Arc, thread};

    use super::{Mutex, RawMutex, owner_spin_eligible};

    #[test]
    fn contended_mutex_wakes_without_lost_handoff() {
        const THREADS: usize = 8;
        const ITERATIONS: usize = 1_000;
        let value = Arc::new(Mutex::new(0usize));
        let workers = (0..THREADS)
            .map(|_| {
                let value = Arc::clone(&value);
                thread::spawn(move || {
                    for _ in 0..ITERATIONS {
                        *value.lock() += 1;
                    }
                })
            })
            .collect::<std::vec::Vec<_>>();

        for worker in workers {
            worker.join().expect("host PI mutex worker panicked");
        }
        assert_eq!(*value.lock(), THREADS * ITERATIONS);
    }

    #[test]
    fn try_lock_does_not_allocate_waiter_state() {
        let mutex = Mutex::new(7usize);
        let guard = mutex.try_lock().expect("uncontended PI try_lock failed");
        let raw = unsafe {
            // SAFETY: this test only observes the raw core while retaining the
            // matching safe guard.
            mutex.raw()
        };
        assert!(
            !raw.core.wait_storage().is_initialized(),
            "uncontended try_lock must not initialize waiter state"
        );
        drop(guard);
    }

    #[test]
    fn owner_spin_requires_every_linux_progress_gate() {
        assert!(owner_spin_eligible(true, true, true, false));
        assert!(!owner_spin_eligible(false, true, true, false));
        assert!(!owner_spin_eligible(true, false, true, false));
        assert!(!owner_spin_eligible(true, true, false, false));
        assert!(!owner_spin_eligible(true, true, true, true));
    }

    #[test]
    fn raw_mutex_remains_const_constructible() {
        static RAW: RawMutex = RawMutex::new();
        assert!(!lock_api::RawMutex::is_locked(&RAW));
    }
}

#[cfg(all(test, target_os = "none"))]
mod tests {
    use alloc::boxed::Box;
    use core::{
        mem::{MaybeUninit, size_of},
        pin::Pin,
    };

    use ax_task::{
        CpuId, PiMutexAcquire, PiMutexCore, PiMutexLockResult, PiWaitToken, SchedulePolicy,
        TaskSystem, TaskSystemConfig, ThreadSpec,
    };

    use super::*;

    fn commit_pi_wait<'lock>(
        system: &TaskSystem,
        lock: &'lock PiMutexCore,
        waiter: ThreadId,
        owner: ThreadId,
    ) -> Result<PiWaitToken<'lock>, TaskError> {
        if !lock.is_owned_by(owner)
            // SAFETY: this scheduler model explicitly establishes `owner` as
            // the physical lock owner before publishing the modeled wait.
            && unsafe { lock.try_acquire_for_thread(owner) }? != PiMutexAcquire::Acquired
        {
            return Err(TaskError::InvalidPiState);
        }
        match system.pi_mutex_lock_slow(lock.mutex_ref()?, waiter, waiter.as_u64())? {
            PiMutexLockResult::Waiting(token) => Ok(token),
            PiMutexLockResult::Acquired => Err(TaskError::InvalidPiState),
        }
    }

    fn unlock_test_owner(raw: &RawMutex) {
        // SAFETY: every caller first installs or acquires the fixture's current
        // thread as this raw mutex's physical owner.
        unsafe { raw.unlock_pi() };
    }

    #[test]
    fn raw_mutex_guard_is_not_send() {
        fn assert_marker<R: lock_api::RawMutex<GuardMarker = lock_api::GuardNoSend>>() {}
        assert_marker::<RawMutex>();
    }

    #[test]
    fn raw_mutex_keeps_linux_rtmutex_style_compact_state() {
        assert!(
            size_of::<RawMutex>() <= 64,
            "PI waiter linkage belongs to threads; the lock object is {} bytes",
            size_of::<RawMutex>()
        );
    }

    #[test]
    fn core_starts_unowned_and_ready() {
        let raw = RawMutex::new();
        assert!(!raw.core.is_locked());
    }

    #[test]
    fn owner_spin_requires_linux_rtmutex_progress_gates() {
        assert!(owner_spin_eligible(true, true, true, false));
        assert!(!owner_spin_eligible(false, true, true, false));
        assert!(!owner_spin_eligible(true, false, true, false));
        assert!(!owner_spin_eligible(true, true, false, false));
        assert!(!owner_spin_eligible(true, true, true, true));
    }

    #[test]
    fn reconstructed_mutex_does_not_reuse_the_previous_pi_identity() {
        let mut storage = MaybeUninit::<RawMutex>::uninit();
        let pointer = storage.as_mut_ptr();

        let (first, second) = unsafe {
            // SAFETY: each write begins a fresh RawMutex lifetime in the same
            // storage, and each initialized value is dropped exactly once.
            pointer.write(RawMutex::new());
            let first = (&*pointer).mutex_ref().id();
            pointer.drop_in_place();
            pointer.write(RawMutex::new());
            let second = (&*pointer).mutex_ref().id();
            pointer.drop_in_place();
            (first, second)
        };

        assert_ne!(
            first, second,
            "a stale scheduler edge must not alias a reconstructed mutex"
        );
    }

    #[test]
    fn pi_identity_stays_stable_for_one_mutex_lifetime() {
        let raw = RawMutex::new();

        assert_eq!(raw.mutex_ref().id(), raw.mutex_ref().id());
    }

    #[test]
    fn uncontended_owner_acquisition_stays_local() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let current = current_thread("test new PI owner registration");

        let attempt = raw.try_or_observe_owner(current.id());

        assert!(matches!(attempt, LockAttempt::Acquired));
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
        assert!(raw.core.is_owned_by(current.id()));
        crate::test_runtime::clear();
    }

    #[test]
    fn slow_registration_reuses_the_captured_current_identity() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let current = task_result(
            ax_task::current_thread_token(),
            "capture PI contender identity",
        );
        crate::test_runtime::reset_cpu_owner_handle_reads();
        crate::test_runtime::reset_preempt_guard_entries();

        raw.lock_contended(&current);

        assert!(raw.core.is_owned_by(current.id()));
        assert_eq!(
            crate::test_runtime::cpu_owner_handle_reads(),
            0,
            "the slow path must not clone a full current handle only to revalidate a stable \
             ThreadId"
        );
        assert_eq!(
            crate::test_runtime::preempt_guard_entries(),
            3,
            "Linux rtmutex registration takes the wait transaction, waiter PI state, and owner PI \
             state under three nested non-sleeping owner scopes"
        );
        crate::test_runtime::clear();
    }

    #[test]
    fn scheduler_selection_precedes_ownerless_claim_grant() {
        let (system, mut cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let owner = current_thread("test PI handoff owner");
        let waiter_thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(waiter_thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), waiter_thread.id()).unwrap();
        let token = commit_pi_wait(&system, &raw.core, waiter_thread.id(), owner.id()).unwrap();
        assert!(token.is_top_waiter());
        assert!(!token.can_claim());
        system
            .pi_mutex_release(raw.mutex_ref(), owner.id())
            .unwrap();
        assert!(token.is_top_waiter());
        assert!(token.can_claim());
        assert!(!token.is_granted());
        system.pi_mutex_claim(&token).unwrap();

        assert!(token.is_granted());
        assert!(raw.core.is_owned_by(waiter_thread.id()));
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
        crate::test_runtime::clear();
    }

    #[test]
    fn contended_unlock_wakes_an_ownerless_claimant() {
        let (system, mut cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let owner = current_thread("test PI release owner");
        let waiter_thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(waiter_thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), waiter_thread.id()).unwrap();
        let token = commit_pi_wait(&system, &raw.core, waiter_thread.id(), owner.id()).unwrap();

        unlock_test_owner(&raw);

        assert!(token.can_claim());
        assert!(
            !token.is_granted(),
            "wake selection is not ownership until the waiter claims locally"
        );
        system.pi_mutex_claim(&token).unwrap();
        assert!(raw.core.is_owned_by(waiter_thread.id()));
        crate::test_runtime::clear();
    }

    #[test]
    fn ownerless_publication_includes_scheduler_selection() {
        let (system, mut cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let owner = current_thread("test atomic PI handoff owner");
        let waiter_thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(waiter_thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), waiter_thread.id()).unwrap();
        let token = commit_pi_wait(&system, &raw.core, waiter_thread.id(), owner.id()).unwrap();
        unlock_test_owner(&raw);

        assert!(token.is_top_waiter());
        assert!(token.can_claim());
        system.pi_mutex_claim(&token).unwrap();
        crate::test_runtime::clear();
    }

    #[test]
    fn selected_waiter_claims_only_after_ownerless_release() {
        let (system, mut cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let owner = current_thread("test PI release owner");
        let waiter_thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(waiter_thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), waiter_thread.id()).unwrap();
        let token = commit_pi_wait(&system, &raw.core, waiter_thread.id(), owner.id()).unwrap();

        unlock_test_owner(&raw);
        assert!(token.is_top_waiter());
        assert!(!token.is_granted());

        system.pi_mutex_claim(&token).unwrap();

        assert!(token.is_granted());
        assert!(raw.core.is_owned_by(waiter_thread.id()));
        crate::test_runtime::clear();
    }

    #[test]
    fn equal_fair_waiter_cannot_steal_ownerless_pi_mutex() {
        let (system, mut cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let owner = current_thread("test PI lateral-steal owner");
        let first_thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        let second_thread = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        for thread in [&first_thread, &second_thread] {
            system.make_ready(thread.id()).unwrap();
            system.enqueue(cpu.as_mut(), thread.id()).unwrap();
        }
        let first_token =
            commit_pi_wait(&system, &raw.core, first_thread.id(), owner.id()).unwrap();
        let second_token =
            commit_pi_wait(&system, &raw.core, second_thread.id(), owner.id()).unwrap();

        unlock_test_owner(&raw);
        assert!(first_token.is_top_waiter());
        assert!(!second_token.is_top_waiter());

        assert!(!second_token.can_claim());
        system.pi_mutex_claim(&first_token).unwrap();

        assert!(!second_token.is_granted());
        assert!(!first_token.is_top_waiter());
        assert!(first_token.is_granted());
        assert!(raw.core.is_owned_by(first_thread.id()));
        system.pi_wait_cancel(second_token).unwrap();
        crate::test_runtime::clear();
    }

    #[test]
    fn uncontended_unlock_releases_without_scheduler_registration() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let owner = current_thread("test uncontended PI owner");
        let contender = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        assert_eq!(
            // SAFETY: this fixture explicitly installs `owner` before testing
            // raw-mutex release without a lock_api guard.
            unsafe { raw.core.try_acquire_for_thread(owner.id()) }.unwrap(),
            PiMutexAcquire::Acquired
        );

        unlock_test_owner(&raw);
        assert!(matches!(
            raw.try_or_observe_owner(contender.id()),
            LockAttempt::Acquired
        ));
        assert!(raw.core.is_owned_by(contender.id()));
        crate::test_runtime::clear();
    }

    #[test]
    fn uncontended_lock_keeps_ownership_local() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let mutex = Mutex::new(7usize);

        let guard = mutex.lock();
        assert!(mutex.is_locked());
        assert!(
            // SAFETY: this test only inspects the raw mutex while retaining the
            // safe guard and performs no raw lock operation.
            unsafe { mutex.raw() }.is_owned_by_current()
        );
        assert_eq!(*guard, 7);
        drop(guard);

        assert!(!mutex.is_locked());
        crate::test_runtime::clear();
    }

    #[test]
    fn uncontended_interruptible_lock_acquires_before_observing_interruption() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let mutex = Mutex::new(7usize);
        let mut interrupt_checks = 0usize;

        let guard = mutex
            .lock_interruptible(|| {
                interrupt_checks += 1;
                true
            })
            .unwrap();

        assert_eq!(*guard, 7);
        assert_eq!(
            interrupt_checks, 0,
            "Linux rtmutex acquisition wins before pending interruption is observed"
        );
        drop(guard);
        crate::test_runtime::clear();
    }

    #[test]
    fn uncontended_lock_does_not_enter_scheduler_irq_facade() {
        const ITERATIONS: usize = 128;

        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let mutex = Mutex::new(0usize);

        for _ in 0..ITERATIONS {
            let mut guard = mutex.lock();
            *guard += 1;
        }

        assert_eq!(
            crate::test_runtime::preempt_guard_entries(),
            ITERATIONS,
            "each uncontended lock pair needs one current capture; RawMutex::unlock already has \
             exclusive owner authority"
        );
        assert_eq!(
            crate::test_runtime::irq_guard_entries(),
            0,
            "Linux rtmutex-style uncontended lock/unlock must remain on the owner-word fast path \
             without entering the scheduler IRQ facade"
        );
        assert_eq!(*mutex.lock(), ITERATIONS);
        crate::test_runtime::clear();
    }

    #[test]
    fn uncontended_lock_does_not_require_a_blocking_context() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        crate::test_runtime::set_schedule_context_safe(false);
        let mutex = Mutex::new(7usize);

        let guard = mutex.lock();
        assert_eq!(*guard, 7);
        drop(guard);

        assert!(!mutex.is_locked());
        crate::test_runtime::clear();
    }

    #[test]
    fn contended_lock_rejects_an_unsafe_blocking_context_before_publication() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let owner = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        assert_eq!(
            // SAFETY: this fixture explicitly installs `owner` to force the
            // current thread through the contended validation path.
            unsafe { raw.core.try_acquire_for_thread(owner.id()) }.unwrap(),
            PiMutexAcquire::Acquired
        );
        crate::test_runtime::set_schedule_context_safe(false);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lock_api::RawMutex::lock(&raw);
        }));

        assert!(result.is_err());
        assert!(raw.core.is_owned_by(owner.id()));
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
        crate::test_runtime::clear();
    }

    #[test]
    fn try_lock_success_and_failure_restore_preemption() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();

        assert!(lock_api::RawMutex::try_lock(&raw));
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
        assert!(!lock_api::RawMutex::try_lock(&raw));
        assert_eq!(crate::test_runtime::preempt_depth(), 0);

        // SAFETY: the first successful try-lock acquisition remains owned by
        // this test thread.
        unsafe { lock_api::RawMutex::unlock(&raw) };
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
        crate::test_runtime::clear();
    }

    fn install_current_thread() -> (Box<TaskSystem>, Pin<Box<ax_task::CpuLocal>>) {
        let system = Box::new(
            TaskSystem::new(TaskSystemConfig::new(1)).expect("test task system should initialize"),
        );
        let mut cpu = system
            .create_cpu_local(CpuId::new(0))
            .expect("test CPU should be valid");
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .expect("bootstrap thread should install");
        system
            .bring_cpu_online(cpu.as_mut())
            .expect("test CPU should come online");
        (system, cpu)
    }
}
