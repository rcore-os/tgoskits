//! Priority-inheritance sleeping mutex.

use core::{
    pin::pin,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use ax_kspin::{PreemptGuard, SpinNoIrq};
use ax_task::{
    PiLockId, PiWaitToken, TaskError, ThreadHandle, ThreadId, ThreadWakeHandle,
    current_thread_handle, current_thread_id, pi_block_current, pi_mutex_handoff, pi_wait_cancel,
    pi_wait_start, pi_wake, validate_blocking_context,
};

use crate::pi::{WaiterNode, WaiterPointer, WaiterQueue};

/// A non-recursive, urgency-ordered PI mutex implementing `lock_api::RawMutex`.
///
/// Only owner and waiter-list metadata is protected by [`SpinNoIrq`]. Every
/// donation, block, scheduler request, handoff, and targeted wake occurs after
/// that metadata guard has been released.
pub struct RawMutex {
    metadata: SpinNoIrq<MutexMetadata>,
    next_waiter_sequence: AtomicU64,
    pending_registrations: AtomicUsize,
    #[cfg(feature = "lockdep")]
    pub(crate) lockdep: crate::lockdep::LockdepMap,
}

#[derive(Debug)]
struct MutexMetadata {
    owner: Option<ThreadId>,
    owner_registered: bool,
    waiters: WaiterQueue,
}

enum LockAttempt<'mutex> {
    Acquired,
    Retry,
    WaitFor(PendingRegistration<'mutex>),
}

struct PendingRegistration<'mutex> {
    mutex: &'mutex RawMutex,
    owner: ThreadId,
    _preempt_guard: PreemptGuard,
}

struct Handoff {
    old_owner: ThreadId,
    selected: Option<WaiterPointer>,
    next_owner: Option<ThreadId>,
    _preempt_guard: PreemptGuard,
}

#[cfg(not(feature = "lockdep"))]
pub type LockSubclass = u32;
#[cfg(feature = "lockdep")]
pub type LockSubclass = crate::lockdep::LockSubclass;

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
            metadata: SpinNoIrq::new(MutexMetadata::new()),
            next_waiter_sequence: AtomicU64::new(0),
            pending_registrations: AtomicUsize::new(0),
            #[cfg(feature = "lockdep")]
            lockdep: crate::lockdep::LockdepMap::new(),
        }
    }

    /// Returns whether the current thread owns this mutex.
    pub fn is_owned_by_current(&self) -> bool {
        let current = current_thread_identity("query PI mutex ownership");
        self.metadata.lock().owner == Some(current)
    }

    #[inline(always)]
    fn lock_id(&self) -> PiLockId {
        PiLockId::new((self as *const Self).expose_provenance())
    }

    fn lock_pi(&self) {
        let current = current_thread_identity("lock PI mutex");
        let mut blocking_context_validated = false;

        loop {
            match self.try_or_observe_owner(current) {
                LockAttempt::Acquired => return,
                LockAttempt::Retry => core::hint::spin_loop(),
                LockAttempt::WaitFor(registration) => {
                    if !blocking_context_validated {
                        // The uncontended path neither publishes a waiter nor
                        // schedules and must remain usable during single-threaded
                        // boot. A contended observation temporarily closes the
                        // owner-registration gate; roll it back before applying
                        // the scheduler's might-sleep contract, then retry from
                        // fresh ownership state after validation.
                        drop(registration);
                        task_result(
                            validate_blocking_context(),
                            "validate PI mutex sleep context",
                        );
                        blocking_context_validated = true;
                        continue;
                    }
                    self.lock_contended(current, registration);
                    return;
                }
            }
        }
    }

    fn lock_contended(&self, current: ThreadId, registration: PendingRegistration<'_>) {
        let current_handle = current_thread("register PI mutex waiter");
        assert_eq!(
            current_handle.id(),
            current,
            "PI mutex contender identity changed while preemption was disabled"
        );
        let sequence = self.next_waiter_sequence.fetch_add(1, Ordering::Relaxed);
        let waiter = pin!(WaiterNode::new(
            current,
            current_handle.effective_scheduling_key(),
            sequence,
            current_handle.clone(),
        ));
        let mut pending_registration = Some(registration);

        loop {
            let registration = match pending_registration.take() {
                Some(registration) => registration,
                None => match self.try_or_observe_owner(current) {
                    LockAttempt::Acquired => return,
                    LockAttempt::Retry => {
                        core::hint::spin_loop();
                        continue;
                    }
                    LockAttempt::WaitFor(registration) => registration,
                },
            };
            if let Some(token) =
                self.register_waiter(&current_handle, waiter.as_ref(), registration)
            {
                self.wait_for_handoff(&waiter, token);
                return;
            }
        }
    }

    fn try_or_observe_owner<'mutex>(&'mutex self, current: ThreadId) -> LockAttempt<'mutex> {
        let mut metadata = self.metadata.lock();
        match metadata.owner {
            None => {
                metadata.owner = Some(current);
                metadata.owner_registered = true;
                LockAttempt::Acquired
            }
            Some(owner) if owner == current => {
                panic!("thread attempted recursive PI mutex acquisition")
            }
            Some(_) if !metadata.owner_registered => LockAttempt::Retry,
            Some(owner) => LockAttempt::WaitFor(self.begin_pending_registration(owner)),
        }
    }

    fn begin_pending_registration(&self, owner: ThreadId) -> PendingRegistration<'_> {
        // The metadata lock already disables preemption. Retain one nested
        // guard after that lock is released so a same-CPU owner cannot preempt
        // this waiter and spin forever waiting for its registration to finish.
        let preempt_guard = PreemptGuard::new();
        let result = self.pending_registrations.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |pending| pending.checked_add(1),
        );
        assert!(result.is_ok(), "PI waiter registration count overflowed");
        PendingRegistration {
            mutex: self,
            owner,
            _preempt_guard: preempt_guard,
        }
    }

    fn register_waiter(
        &self,
        current: &ThreadHandle,
        waiter: core::pin::Pin<&WaiterNode>,
        registration: PendingRegistration<'_>,
    ) -> Option<PiWaitToken> {
        self.register_waiter_after(current, waiter, registration, || {})
    }

    fn register_waiter_after(
        &self,
        current: &ThreadHandle,
        waiter: core::pin::Pin<&WaiterNode>,
        registration: PendingRegistration<'_>,
        after_registration: impl FnOnce(),
    ) -> Option<PiWaitToken> {
        let owner = registration.owner;
        let token = match pi_wait_start(self.lock_id(), owner) {
            Ok(token) => token,
            // Ownership changed between observing metadata and registering the
            // scheduler donation. No local waiter pointer was published yet,
            // so retry against the new owner.
            Err(TaskError::InvalidPiState) => return None,
            Err(error) => panic!("start PI mutex wait failed: {error}"),
        };
        after_registration();

        let inserted = {
            let mut metadata = self.metadata.lock();
            if metadata.owner == Some(owner) && metadata.owner_registered && current.id() != owner {
                // SAFETY: scheduler donation is already registered, `lock_pi`
                // keeps this stack node pinned until handoff, and the metadata
                // guard exclusively owns the intrusive list.
                unsafe { metadata.waiters.insert(waiter) };
                true
            } else {
                false
            }
        };
        if inserted {
            Some(token)
        } else {
            task_result(pi_wait_cancel(token), "cancel overtaken PI mutex wait");
            None
        }
    }

    fn wait_for_handoff(&self, waiter: &core::pin::Pin<&mut WaiterNode>, token: PiWaitToken) {
        if token.is_granted() {
            wait_for_local_grant(waiter);
            return;
        }
        task_result(pi_block_current(&token), "block on PI mutex");
        wait_for_local_grant(waiter);
    }

    fn try_lock_pi(&self) -> bool {
        let current = current_thread_identity("try PI mutex");
        let mut metadata = self.metadata.lock();
        if metadata.owner.is_some() {
            return false;
        }
        metadata.owner = Some(current);
        metadata.owner_registered = true;
        true
    }

    fn unlock_pi(&self) {
        let current = current_thread_identity("unlock PI mutex");
        #[cfg(feature = "lockdep")]
        crate::lockdep::release(self);

        let mut handoff = self.prepare_handoff(current);
        let wake = if handoff.next_owner.is_some() {
            // A selected waiter may observe its scheduler token immediately
            // after handoff, so publish local ownership and the stack-pinned
            // grant before making that token visible.
            let wake = self.publish_local_handoff(&mut handoff);
            task_result(
                pi_mutex_handoff(self.lock_id(), handoff.old_owner, handoff.next_owner),
                "complete PI mutex handoff",
            );
            wake
        } else {
            // Uncontended ownership never enters the scheduler PI graph, so a
            // local release is the complete fast path.
            self.publish_local_handoff(&mut handoff)
        };
        if let Some(wake) = wake {
            task_result(pi_wake(&wake), "wake selected PI mutex waiter");
        }
    }

    fn prepare_handoff(&self, current: ThreadId) -> Handoff {
        // Keep the current owner running from closing the waiter-registration
        // gate through scheduler and local ownership publication. Otherwise a
        // same-CPU contender can preempt here and spin on the closed gate,
        // preventing this owner from completing the handoff.
        let preempt_guard = PreemptGuard::new();
        let head = self.freeze_waiter_list(current);
        let selected = select_most_urgent_waiter(head);
        let next_owner = selected.as_ref().map(|waiter| {
            // SAFETY: the selected waiter remains pinned and cannot leave its
            // lock call frame before `publish_handoff` grants it.
            unsafe { waiter.thread_id() }
        });

        let mut metadata = self.metadata.lock();
        assert_eq!(metadata.owner, Some(current));
        assert!(!metadata.owner_registered);
        let selected = selected.map(|waiter| {
            metadata
                .waiters
                .remove(&waiter)
                .expect("frozen PI waiter must remain queued")
        });
        metadata.owner = next_owner.or(Some(current));
        Handoff {
            old_owner: current,
            selected,
            next_owner,
            _preempt_guard: preempt_guard,
        }
    }

    fn freeze_waiter_list(&self, current: ThreadId) -> Option<WaiterPointer> {
        {
            let mut metadata = self.metadata.lock();
            assert_eq!(
                metadata.owner,
                Some(current),
                "thread attempted to unlock a PI mutex it does not own"
            );
            assert!(
                metadata.owner_registered,
                "unregistered PI mutex owner unlocked"
            );
            // Closing this gate while holding metadata prevents any later
            // waiter from entering the scheduler registration window.
            metadata.owner_registered = false;
        }
        while self.pending_registrations.load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
        let metadata = self.metadata.lock();
        assert_eq!(metadata.owner, Some(current));
        assert!(!metadata.owner_registered);
        metadata.waiters.head()
    }

    fn publish_local_handoff(&self, handoff: &mut Handoff) -> Option<ThreadWakeHandle> {
        {
            let mut metadata = self.metadata.lock();
            match handoff.next_owner {
                Some(next) => {
                    assert_eq!(metadata.owner, Some(next));
                    metadata.owner_registered = true;
                }
                None => {
                    assert_eq!(metadata.owner, Some(handoff.old_owner));
                    metadata.owner = None;
                    metadata.owner_registered = true;
                }
            }
        }

        handoff.selected.take().map(|waiter| {
            // SAFETY: handoff has completed before grant, and the waiter cannot
            // leave its pinned frame until this Release store is observed.
            unsafe {
                waiter.grant();
                waiter.wake_handle()
            }
            .expect("production PI waiters always carry a wake handle")
        })
    }

    #[cfg(feature = "lockdep")]
    #[track_caller]
    fn lock_nested(&self, subclass: LockSubclass) {
        let lockdep = crate::lockdep::LockdepAcquire::prepare_nested(self, false, subclass);
        self.lock_pi();
        lockdep.finish(true);
    }

    #[cfg(feature = "lockdep")]
    #[track_caller]
    fn try_lock_nested(&self, subclass: LockSubclass) -> bool {
        let lockdep = crate::lockdep::LockdepAcquire::prepare_nested(self, true, subclass);
        let acquired = self.try_lock_pi();
        lockdep.finish(acquired);
        acquired
    }
}

impl Drop for PendingRegistration<'_> {
    fn drop(&mut self) {
        let result = self.mutex.pending_registrations.fetch_update(
            Ordering::Release,
            Ordering::Relaxed,
            |pending| pending.checked_sub(1),
        );
        assert!(result.is_ok(), "PI waiter registration count underflowed");
    }
}

impl MutexMetadata {
    const fn new() -> Self {
        Self {
            owner: None,
            owner_registered: true,
            waiters: WaiterQueue::new(),
        }
    }
}

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: metadata transitions are serialized by an IRQ/preemption-aware spin
// lock. A lock_api guard is created only after scheduler ownership registration
// or an explicit PI handoff grants the calling thread.
unsafe impl lock_api::RawMutex for RawMutex {
    type GuardMarker = lock_api::GuardNoSend;

    const INIT: Self = Self::new();

    #[inline(always)]
    #[track_caller]
    fn lock(&self) {
        #[cfg(feature = "lockdep")]
        self.lock_nested(ax_lockdep::DEFAULT_LOCK_SUBCLASS);

        #[cfg(not(feature = "lockdep"))]
        self.lock_pi();
    }

    #[inline(always)]
    #[track_caller]
    fn try_lock(&self) -> bool {
        #[cfg(feature = "lockdep")]
        {
            self.try_lock_nested(ax_lockdep::DEFAULT_LOCK_SUBCLASS)
        }

        #[cfg(not(feature = "lockdep"))]
        {
            self.try_lock_pi()
        }
    }

    #[inline(always)]
    unsafe fn unlock(&self) {
        self.unlock_pi();
    }

    #[inline(always)]
    fn is_locked(&self) -> bool {
        self.metadata.lock().owner.is_some()
    }
}

fn current_thread(operation: &'static str) -> ThreadHandle {
    task_result(current_thread_handle(), operation)
}

fn current_thread_identity(operation: &'static str) -> ThreadId {
    task_result(current_thread_id(), operation)
}

fn task_result<T>(result: Result<T, TaskError>, operation: &'static str) -> T {
    result.unwrap_or_else(|error| panic!("{operation} failed: {error}"))
}

fn select_most_urgent_waiter(head: Option<WaiterPointer>) -> Option<WaiterPointer> {
    let mut current = head;
    let mut selected: Option<(WaiterPointer, (ax_task::SchedulingKey, u64))> = None;
    while let Some(waiter) = current {
        // SAFETY: `freeze_waiter_list` prevents enqueue/removal and every
        // blocked waiter remains pinned until handoff publishes grant.
        let (key, next) = unsafe { (waiter.effective_ordering_key(), waiter.next()) };
        if selected
            .as_ref()
            .is_none_or(|(_, selected_key)| key < *selected_key)
        {
            selected = Some((waiter, key));
        }
        current = next;
    }
    selected.map(|(waiter, _)| waiter)
}

fn wait_for_local_grant(waiter: &core::pin::Pin<&mut WaiterNode>) {
    while !waiter.is_granted() {
        core::hint::spin_loop();
    }
}

/// A safe PI mutex using [`RawMutex`].
pub type Mutex<T> = lock_api::Mutex<RawMutex, T>;
/// A non-send guard returned by [`Mutex`].
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawMutex, T>;

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::pin::Pin;

    use ax_task::{CpuId, SchedulePolicy, TaskSystem, TaskSystemConfig, ThreadSpec};

    use super::*;

    #[test]
    fn raw_mutex_guard_is_not_send() {
        fn assert_marker<R: lock_api::RawMutex<GuardMarker = lock_api::GuardNoSend>>() {}
        assert_marker::<RawMutex>();
    }

    #[test]
    fn metadata_starts_unowned_and_ready() {
        let metadata = MutexMetadata::new();
        assert_eq!(metadata.owner, None);
        assert!(metadata.owner_registered);
        assert!(metadata.waiters.is_empty());
    }

    #[test]
    fn pending_waiter_registration_disables_preemption_until_publication_finishes() {
        let _runtime = crate::test_runtime::install(0, 0);
        let raw = RawMutex::new();
        let registration = raw.begin_pending_registration(ThreadId::from_parts(1, 1));

        assert_eq!(crate::test_runtime::preempt_depth(), 1);
        drop(registration);
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
        crate::test_runtime::clear();
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
        let metadata = raw.metadata.lock();
        assert_eq!(metadata.owner, Some(current.id()));
        assert!(metadata.owner_registered);
        drop(metadata);
        crate::test_runtime::clear();
    }

    #[test]
    fn owner_handoff_disables_preemption_until_local_publication() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let current = current_thread("test PI handoff publication");
        {
            let mut metadata = raw.metadata.lock();
            metadata.owner = Some(current.id());
            metadata.owner_registered = true;
        }

        let handoff = raw.prepare_handoff(current.id());

        assert_eq!(crate::test_runtime::preempt_depth(), 1);
        drop(handoff);
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
        crate::test_runtime::clear();
    }

    #[test]
    fn scheduler_grant_is_not_visible_before_local_handoff_publication() {
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
        system.enqueue(cpu.as_mut(), waiter_thread.id(), 0).unwrap();
        let token = system
            .pi_wait_start(raw.lock_id(), waiter_thread.id(), owner.id())
            .unwrap();
        let waiter = pin!(WaiterNode::new(
            waiter_thread.id(),
            waiter_thread.effective_scheduling_key(),
            0,
            waiter_thread.clone(),
        ));
        {
            let mut metadata = raw.metadata.lock();
            metadata.owner = Some(owner.id());
            metadata.owner_registered = true;
            // SAFETY: this pinned waiter remains live until the test removes it
            // through the handoff prepared below.
            unsafe { metadata.waiters.insert(waiter.as_ref()) };
        }

        let mut handoff = raw.prepare_handoff(owner.id());
        let _wake = raw.publish_local_handoff(&mut handoff);
        assert!(waiter.is_granted());
        assert!(!token.is_granted());
        system
            .pi_mutex_handoff(raw.lock_id(), owner.id(), Some(waiter_thread.id()))
            .unwrap();

        assert!(token.is_granted());
        assert!(waiter.is_granted());
        drop(handoff);
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
        {
            let mut metadata = raw.metadata.lock();
            metadata.owner = Some(owner.id());
            metadata.owner_registered = true;
        }

        raw.unlock_pi();
        assert!(matches!(
            raw.try_or_observe_owner(contender.id()),
            LockAttempt::Acquired
        ));
        assert_eq!(raw.metadata.lock().owner, Some(contender.id()));
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
        {
            let mut metadata = raw.metadata.lock();
            metadata.owner = Some(owner.id());
            metadata.owner_registered = true;
        }
        crate::test_runtime::set_schedule_context_safe(false);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lock_api::RawMutex::lock(&raw);
        }));

        assert!(result.is_err());
        assert_eq!(raw.pending_registrations.load(Ordering::Acquire), 0);
        assert!(raw.metadata.lock().waiters.is_empty());
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

    #[test]
    fn unlock_overtaking_donation_registration_retries_without_stale_waiter() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let waiter_thread = current_thread("test PI waiter");
        let owner = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        {
            let mut metadata = raw.metadata.lock();
            metadata.owner = Some(owner.id());
            metadata.owner_registered = true;
        }
        let waiter = pin!(WaiterNode::new(
            waiter_thread.id(),
            waiter_thread.effective_scheduling_key(),
            0,
            waiter_thread.clone(),
        ));

        let registration = {
            let metadata = raw.metadata.lock();
            assert_eq!(metadata.owner, Some(owner.id()));
            assert!(metadata.owner_registered);
            raw.begin_pending_registration(owner.id())
        };
        let token =
            raw.register_waiter_after(&waiter_thread, waiter.as_ref(), registration, || {
                raw.metadata.lock().owner_registered = false;
            });

        assert!(token.is_none());
        assert_eq!(raw.pending_registrations.load(Ordering::Acquire), 0);
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
        system
            .pi_mutex_handoff(raw.lock_id(), owner.id(), None)
            .unwrap();
        {
            let mut metadata = raw.metadata.lock();
            metadata.owner = None;
            metadata.owner_registered = true;
            assert_eq!(metadata.owner, None);
            assert!(metadata.owner_registered);
            assert!(metadata.waiters.is_empty());
        }
        lock_api::RawMutex::lock(&raw);
        assert!(raw.is_owned_by_current());
        // SAFETY: this test owns the raw acquisition immediately above.
        unsafe { lock_api::RawMutex::unlock(&raw) };
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
