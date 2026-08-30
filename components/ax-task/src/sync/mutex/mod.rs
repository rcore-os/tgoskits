//! Priority-inheritance sleeping mutex.

use core::sync::atomic::{AtomicU64, Ordering};

mod entry;
#[cfg(feature = "lockdep")]
pub(in crate::sync) mod lockdep;
#[path = "core.rs"]
mod pi_core;

use self::entry::{
    FastLockAttempt, LockEntry, capture_current_and_prepare_slow, owner_spin_eligible,
    owner_spin_progress_gates,
};
pub use self::pi_core::*;

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
    pub(crate) lockdep: super::lockdep::LockdepMap,
}

/// Borrowed execution state for the unique native PI-mutex algorithm.
pub(in crate::sync) struct PiMutexAlgorithm<'lock> {
    core: PiMutexCoreView<'lock>,
    next_waiter_sequence: &'lock AtomicU64,
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
    /// `mutex_lock_interruptible()` under PREEMPT_RT. The interruption
    /// publisher must also wake the waiting task, just as Linux
    /// `signal_wake_up_state()` sets `TIF_SIGPENDING` and wakes
    /// `TASK_INTERRUPTIBLE`; this predicate is not polled while the task is
    /// asleep.
    fn lock_interruptible<F>(
        &self,
        should_interrupt: F,
    ) -> Result<MutexGuard<'_, T>, PiMutexLockInterrupted>
    where
        F: FnMut() -> bool;
}

#[cfg(not(feature = "lockdep"))]
/// A lockdep subclass identifier when lockdep is disabled.
pub type LockSubclass = u32;
#[cfg(feature = "lockdep")]
pub type LockSubclass = super::lockdep::LockSubclass;

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
            lockdep: super::lockdep::LockdepMap::new(),
        }
    }

    const fn algorithm(&self) -> PiMutexAlgorithm<'_> {
        PiMutexAlgorithm::new(self.core.view(), &self.next_waiter_sequence)
    }

    /// Returns whether the current thread owns this mutex.
    pub fn is_owned_by_current(&self) -> bool {
        self.algorithm().is_owned_by_current()
    }
}

impl<'lock> PiMutexAlgorithm<'lock> {
    pub(in crate::sync) const fn new(
        core: PiMutexCoreView<'lock>,
        next_waiter_sequence: &'lock AtomicU64,
    ) -> Self {
        Self {
            core,
            next_waiter_sequence,
        }
    }

    pub(in crate::sync) fn is_owned_by_current(&self) -> bool {
        Self::core_is_owned_by_current(self.core)
    }

    pub(in crate::sync) fn core_is_owned_by_current(core: PiMutexCoreView<'_>) -> bool {
        core.is_owned_by(Self::current_task_id())
    }

    #[inline(always)]
    fn current_task_id() -> PiTaskId {
        task_result(crate::current_thread_id(), "capture current PI mutex task").into()
    }

    pub(in crate::sync) fn lock_pi(&self) {
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_pi_mutex_lock_attempt();
        match capture_current_and_prepare_slow(
            || {
                task_result(
                    crate::current_thread_token(),
                    "capture current PI mutex task",
                )
            },
            |current| self.try_or_observe_current_token(current.id().into()),
            || {
                // The uncontended path neither publishes a waiter nor
                // schedules and must remain usable during single-threaded
                // boot.
                task_result(
                    crate::validate_blocking_context(),
                    "validate PI mutex blocking context",
                );
            },
        ) {
            LockEntry::Acquired => {
                #[cfg(feature = "qperf-metrics")]
                crate::metrics::record_pi_mutex_fast_acquisition();
            }
            LockEntry::Contended(current) => {
                #[cfg(feature = "qperf-metrics")]
                crate::metrics::record_pi_mutex_slow_entry();
                self.lock_contended(current);
            }
        }
    }

    fn lock_pi_interruptible(
        &self,
        mut should_interrupt: impl FnMut() -> bool,
    ) -> Result<(), PiMutexLockInterrupted> {
        match capture_current_and_prepare_slow(
            || {
                task_result(
                    crate::current_thread_token(),
                    "capture current PI mutex task",
                )
            },
            |current| self.try_or_observe_current_token(current.id().into()),
            || {
                task_result(
                    crate::validate_blocking_context(),
                    "validate PI mutex blocking context",
                );
            },
        ) {
            LockEntry::Acquired => Ok(()),
            LockEntry::Contended(current) => {
                self.lock_contended_interruptible(current, &mut should_interrupt)
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn lock_contended(&self, current: crate::CurrentThreadToken) {
        let current_id = current.id().into();
        let sequence = self.next_waiter_sequence.fetch_add(1, Ordering::Relaxed);
        let lock = core_result(self.core.mutex_ref(), "borrow PI mutex identity");
        let token = match task_result(
            crate::pi_mutex_lock_slow(lock, &current, sequence),
            "register PI mutex waiter",
        ) {
            PiMutexLockResult::Acquired => {
                #[cfg(feature = "qperf-metrics")]
                crate::metrics::record_pi_mutex_slow_race_acquisition();
                return;
            }
            PiMutexLockResult::Waiting(token) => {
                #[cfg(feature = "qperf-metrics")]
                crate::metrics::record_pi_mutex_waiter_registration();
                token
            }
        };
        debug_assert_eq!(token.thread_id(), current_id);
        if self.try_claim_waiter(&token, &current) {
            return;
        }
        self.wait_for_handoff(token, &current);
    }

    #[cold]
    #[inline(never)]
    fn lock_contended_interruptible(
        &self,
        current: crate::CurrentThreadToken,
        should_interrupt: &mut impl FnMut() -> bool,
    ) -> Result<(), PiMutexLockInterrupted> {
        let current_id = current.id().into();
        let sequence = self.next_waiter_sequence.fetch_add(1, Ordering::Relaxed);
        let lock = core_result(self.core.mutex_ref(), "borrow PI mutex identity");
        let token = match task_result(
            crate::pi_mutex_lock_slow(lock, &current, sequence),
            "register interruptible PI mutex waiter",
        ) {
            PiMutexLockResult::Acquired => return Ok(()),
            PiMutexLockResult::Waiting(token) => token,
        };
        debug_assert_eq!(token.thread_id(), current_id);

        loop {
            if token.is_granted() || self.try_claim_waiter(&token, &current) {
                return Ok(());
            }
            if should_interrupt() {
                match task_result(
                    crate::pi_wait_try_cancel(&token),
                    "cancel interruptible PI mutex waiter",
                ) {
                    PiWaitCancelOutcome::Cancelled => return Err(PiMutexLockInterrupted),
                    PiWaitCancelOutcome::HandoffPending => continue,
                }
            }
            if !token.can_claim() && !self.spin_on_owner(&token) {
                task_result(
                    crate::pi_park_current_once(&token),
                    "park interruptible PI mutex waiter",
                );
            }
        }
    }

    fn wait_for_handoff(&self, token: PiWaitToken, current: &crate::CurrentThreadToken) {
        loop {
            if token.is_granted() {
                break;
            }
            if self.try_claim_waiter(&token, current) {
                break;
            }
            if !token.can_claim() && !self.spin_on_owner(&token) {
                #[cfg(feature = "qperf-metrics")]
                crate::metrics::record_pi_mutex_waiter_park();
                task_result(crate::pi_park_current_once(&token), "park PI mutex waiter");
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
    /// no pending reschedule request. The architecture current-state query is
    /// advisory and leaves owner spinning preemptible.
    fn spin_on_owner(&self, token: &PiWaitToken) -> bool {
        let Some(owner) = token.initial_owner() else {
            return token.can_claim() || token.is_granted();
        };
        let cpu_count = task_result(crate::cpu_topology_len(), "capture PI mutex CPU topology");

        loop {
            if token.can_claim() || token.is_granted() {
                return true;
            }

            let may_spin = owner_spin_eligible(cpu_count, || {
                owner_spin_progress_gates(
                    self.core.is_owned_by(owner),
                    token.initial_owner_is_on_cpu(),
                    token.is_top_waiter(),
                    crate::runtime::task_runtime::current_preemption_pending(),
                )
            });
            if !may_spin {
                return token.can_claim() || token.is_granted();
            }

            core::hint::spin_loop();
        }
    }

    fn try_or_observe_current_token(&self, current: PiTaskId) -> FastLockAttempt {
        match core_result(self.core.try_acquire(current), "try PI mutex acquisition") {
            PiMutexAcquire::Acquired => FastLockAttempt::Acquired,
            PiMutexAcquire::Contended => FastLockAttempt::Contended,
        }
    }

    pub(in crate::sync) fn try_lock_pi(&self) -> bool {
        let current = Self::current_task_id();
        match self.core.try_acquire(current) {
            Ok(PiMutexAcquire::Acquired) => true,
            Ok(PiMutexAcquire::Contended) | Err(PiMutexStateError::WaiterOwnsLock) => false,
            Err(error) => panic!("try PI mutex failed: {error}"),
        }
    }

    fn try_claim_waiter(&self, token: &PiWaitToken, current: &crate::CurrentThreadToken) -> bool {
        if token.is_granted() {
            return true;
        }
        if !token.can_claim() {
            return false;
        }
        match task_result(
            crate::pi_mutex_claim(token, current),
            "claim ownerless PI mutex handoff",
        ) {
            PiMutexClaimOutcome::Claimed => true,
            PiMutexClaimOutcome::Retry => false,
        }
    }

    pub(in crate::sync) unsafe fn unlock_pi(&self) {
        // SAFETY: forwarded from this method's raw-mutex ownership contract.
        unsafe { Self::unlock_core(self.core) };
    }

    pub(in crate::sync) unsafe fn unlock_core(core: PiMutexCoreView<'_>) {
        let current = Self::current_task_id();
        // SAFETY: the caller is the lock_api raw-mutex owner and retains that
        // exclusive authority through this complete release transaction;
        // `current` is the executing scheduler identity named by that owner
        // contract.
        match core_result(
            unsafe { core.try_release_owned(current) },
            "try PI mutex release",
        ) {
            PiMutexOwnedRelease::Released => {}
            PiMutexOwnedRelease::Contended(owner) => {
                #[cfg(feature = "qperf-metrics")]
                crate::metrics::record_pi_mutex_contended_release();
                // SAFETY: `owner` came from this core's owner-authorized release
                // result and the raw-mutex contract remains active.
                unsafe { Self::unlock_contended(core, owner) };
            }
        }
    }

    unsafe fn unlock_contended(core: PiMutexCoreView<'_>, owner: PiTaskId) {
        let lock = core_result(core.mutex_ref(), "borrow PI mutex release identity");
        task_result(
            unsafe {
                // SAFETY: `owner` came from this core's owner-authorized
                // release transition, and the raw-mutex contract remains held.
                crate::pi_mutex_release_owned(lock, owner.into())
            },
            "release contended PI mutex",
        );
    }

    pub(in crate::sync) fn is_locked(&self) -> bool {
        Self::core_is_locked(self.core)
    }

    pub(in crate::sync) fn core_is_locked(core: PiMutexCoreView<'_>) -> bool {
        core.is_locked()
    }
}

impl RawMutex {
    fn lock_pi(&self) {
        self.algorithm().lock_pi();
    }

    fn lock_pi_interruptible(
        &self,
        should_interrupt: impl FnMut() -> bool,
    ) -> Result<(), PiMutexLockInterrupted> {
        self.algorithm().lock_pi_interruptible(should_interrupt)
    }

    fn try_lock_pi(&self) -> bool {
        self.algorithm().try_lock_pi()
    }

    unsafe fn unlock_pi(&self) {
        // SAFETY: forwarded from the caller's raw-mutex ownership contract.
        unsafe { self.algorithm().unlock_pi() };
    }

    #[cfg(feature = "lockdep")]
    #[track_caller]
    fn lock_nested(&self, subclass: LockSubclass) {
        let lockdep = lockdep::LockdepAcquire::prepare_nested(self, false, subclass);
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
        let lockdep = lockdep::LockdepAcquire::prepare_nested(self, false, subclass);
        let result = self.lock_pi_interruptible(should_interrupt);
        lockdep.finish(result.is_ok());
        result
    }

    #[cfg(feature = "lockdep")]
    #[track_caller]
    fn try_lock_nested(&self, subclass: LockSubclass) -> bool {
        let lockdep = lockdep::LockdepAcquire::prepare_nested(self, true, subclass);
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
        self.lock_nested(super::lockdep::DEFAULT_LOCK_SUBCLASS);

        #[cfg(not(feature = "lockdep"))]
        self.lock_pi();
    }

    #[inline(always)]
    #[track_caller]
    fn try_lock(&self) -> bool {
        #[cfg(feature = "lockdep")]
        {
            self.try_lock_nested(super::lockdep::DEFAULT_LOCK_SUBCLASS)
        }

        #[cfg(not(feature = "lockdep"))]
        {
            self.try_lock_pi()
        }
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        #[cfg(feature = "lockdep")]
        lockdep::release(self);
        // SAFETY: lock_api calls `unlock` only for the execution context that
        // owns this raw mutex, and this method consumes that ownership once.
        unsafe { self.unlock_pi() };
    }

    #[inline(always)]
    fn is_locked(&self) -> bool {
        self.algorithm().is_locked()
    }
}

#[track_caller]
fn core_result<T>(result: Result<T, PiMutexStateError>, operation: &'static str) -> T {
    result.unwrap_or_else(|error| panic!("{operation} failed: {error}"))
}

#[track_caller]
pub(super) fn task_result<T, E>(result: Result<T, E>, operation: &'static str) -> T
where
    E: core::fmt::Display,
{
    result.unwrap_or_else(|error| panic!("{operation} failed: {error}"))
}

/// A safe PI mutex using [`RawMutex`].
pub type Mutex<T> = lock_api::Mutex<RawMutex, T>;
/// A non-send guard returned by [`Mutex`].
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawMutex, T>;
/// Explicit name for the scheduler-owned priority-inheritance mutex.
pub type PiMutex<T> = Mutex<T>;
/// Explicit guard name for [`PiMutex`].
pub type PiMutexGuard<'a, T> = MutexGuard<'a, T>;
/// Raw priority-inheritance mutex used by [`PiMutex`].
pub type RawPiMutex = RawMutex;

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
        raw.lock_interruptible_nested(super::lockdep::DEFAULT_LOCK_SUBCLASS, should_interrupt)?;
        #[cfg(not(feature = "lockdep"))]
        raw.lock_pi_interruptible(should_interrupt)?;

        // SAFETY: the raw acquisition above established current as owner.
        Ok(unsafe { self.make_guard_unchecked() })
    }
}
