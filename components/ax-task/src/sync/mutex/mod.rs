//! Priority-inheritance sleeping mutex.

use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "host-test")]
static HOST_BLOCKING_CONTEXT_VALIDATIONS: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "lockdep")]
pub(in crate::sync) mod lockdep;
#[path = "core.rs"]
mod pi_core;

pub use self::pi_core::*;
use super::context::PreemptGuard;

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

    #[inline(always)]
    #[cfg(all(test, target_os = "none"))]
    fn mutex_ref(&self) -> PiMutexRef<'_> {
        core_result(self.core.mutex_ref(), "borrow PI mutex core")
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
                        #[cfg(feature = "host-test")]
                        HOST_BLOCKING_CONTEXT_VALIDATIONS.fetch_add(1, Ordering::Relaxed);
                        task_result(
                            crate::validate_blocking_context(),
                            "validate PI mutex blocking context",
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
                        task_result(
                            crate::validate_blocking_context(),
                            "validate PI mutex blocking context",
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
        let current_token = task_result(crate::current_thread_token(), "capture PI mutex waiter");
        debug_assert_eq!(current_token.id(), current.into());
        let lock = core_result(self.core.mutex_ref(), "borrow PI mutex identity");
        let token = match task_result(
            crate::pi_mutex_lock_slow(lock, &current_token, sequence),
            "register PI mutex waiter",
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
        let current_token = task_result(
            crate::current_thread_token(),
            "capture interruptible PI mutex waiter",
        );
        debug_assert_eq!(current_token.id(), current.into());
        let lock = core_result(self.core.mutex_ref(), "borrow PI mutex identity");
        let token = match task_result(
            crate::pi_mutex_lock_slow(lock, &current_token, sequence),
            "register interruptible PI mutex waiter",
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

    fn wait_for_handoff(&self, token: PiWaitToken) {
        loop {
            if token.is_granted() {
                break;
            }
            if self.try_claim_waiter(&token) {
                break;
            }
            if !token.can_claim() && !self.spin_on_owner(&token) {
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
            let need_resched = task_result(
                unsafe {
                    // SAFETY: `_preempt_guard` pins this caller through the
                    // complete owner-spin eligibility decision.
                    crate::current_needs_reschedule_pinned()
                },
                "observe pinned PI mutex reschedule state",
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

    pub(in crate::sync) fn try_lock_pi(&self) -> bool {
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
        let current = task_result(crate::current_thread_token(), "capture PI mutex claimant");
        match task_result(
            crate::pi_mutex_claim(token, &current),
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
        // SAFETY: the caller is the lock_api raw-mutex owner and retains that
        // exclusive authority through this complete release transaction.
        match core_result(unsafe { core.try_release_owned() }, "try PI mutex release") {
            PiMutexOwnedRelease::Released => {}
            PiMutexOwnedRelease::Contended(owner) => {
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

#[cfg(feature = "host-test")]
pub(crate) fn host_blocking_context_validations() -> u64 {
    HOST_BLOCKING_CONTEXT_VALIDATIONS.load(Ordering::Relaxed)
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

#[cfg(test)]
mod tests {
    use super::{RawMutex, owner_spin_eligible};

    #[test]
    fn raw_mutex_guard_is_not_send() {
        fn assert_marker<R: lock_api::RawMutex<GuardMarker = lock_api::GuardNoSend>>() {}
        assert_marker::<RawMutex>();
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
