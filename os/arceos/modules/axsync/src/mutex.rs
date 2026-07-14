//! Priority-inheritance sleeping mutex.

use core::{
    pin::pin,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

use ax_kspin::{PreemptGuard, SpinNoIrq};
use ax_task::{
    PiLockId, PiWaitToken, TaskError, ThreadHandle, ThreadId, current_thread_handle,
    pi_block_current, pi_mutex_acquired, pi_mutex_handoff, pi_wait_cancel, pi_wait_start, pi_wake,
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
    Acquired(PreemptGuard),
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
        let current = current_thread("query PI mutex ownership");
        self.metadata.lock().owner == Some(current.id())
    }

    #[inline(always)]
    fn lock_id(&self) -> PiLockId {
        PiLockId::new((self as *const Self).expose_provenance())
    }

    fn lock_pi(&self) {
        let current = current_thread("lock PI mutex");
        let sequence = self.next_waiter_sequence.fetch_add(1, Ordering::Relaxed);
        let waiter = pin!(WaiterNode::new(
            current.id(),
            current.effective_scheduling_key(),
            sequence,
            current.clone(),
        ));

        loop {
            match self.try_or_observe_owner(&current) {
                LockAttempt::Acquired(registration_guard) => {
                    self.register_uncontended_owner(current.id());
                    drop(registration_guard);
                    return;
                }
                LockAttempt::Retry => core::hint::spin_loop(),
                LockAttempt::WaitFor(registration) => {
                    if let Some(token) =
                        self.register_waiter(&current, waiter.as_ref(), registration)
                    {
                        self.wait_for_handoff(&waiter, token);
                        return;
                    }
                }
            }
        }
    }

    fn try_or_observe_owner<'mutex>(&'mutex self, current: &ThreadHandle) -> LockAttempt<'mutex> {
        let mut metadata = self.metadata.lock();
        match metadata.owner {
            None => {
                metadata.owner = Some(current.id());
                metadata.owner_registered = false;
                LockAttempt::Acquired(PreemptGuard::new())
            }
            Some(owner) if owner == current.id() => {
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

    fn register_uncontended_owner(&self, owner: ThreadId) {
        task_result(
            pi_mutex_acquired(self.lock_id(), owner),
            "register PI mutex owner",
        );
        let mut metadata = self.metadata.lock();
        assert_eq!(
            metadata.owner,
            Some(owner),
            "PI mutex owner changed while registering"
        );
        assert!(
            !metadata.owner_registered,
            "PI mutex owner registered twice"
        );
        metadata.owner_registered = true;
    }

    fn wait_for_handoff(&self, waiter: &core::pin::Pin<&mut WaiterNode>, token: PiWaitToken) {
        if waiter.is_granted() {
            return;
        }
        if token.is_granted() {
            wait_for_local_grant(waiter);
            return;
        }
        if waiter.is_granted() {
            task_result(pi_wait_cancel(token), "cancel raced PI mutex wait");
            return;
        }
        task_result(pi_block_current(&token), "block on PI mutex");
        wait_for_local_grant(waiter);
    }

    fn try_lock_pi(&self) -> bool {
        let current = current_thread("try PI mutex");
        let registration_guard = {
            let mut metadata = self.metadata.lock();
            if metadata.owner.is_some() {
                return false;
            }
            metadata.owner = Some(current.id());
            metadata.owner_registered = false;
            PreemptGuard::new()
        };
        self.register_uncontended_owner(current.id());
        drop(registration_guard);
        true
    }

    fn unlock_pi(&self) {
        let current = current_thread("unlock PI mutex");
        #[cfg(feature = "lockdep")]
        crate::lockdep::release(self);

        let handoff = self.prepare_handoff(current.id());
        task_result(
            pi_mutex_handoff(self.lock_id(), handoff.old_owner, handoff.next_owner),
            "complete PI mutex handoff",
        );
        self.publish_handoff(handoff);
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

    fn publish_handoff(&self, handoff: Handoff) {
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

        if let Some(waiter) = handoff.selected {
            // SAFETY: handoff has completed before grant, and the waiter cannot
            // leave its pinned frame until this Release store is observed.
            let wake = unsafe {
                waiter.grant();
                waiter.wake_handle()
            };
            let wake = wake.expect("production PI waiters always carry a wake handle");
            task_result(pi_wake(&wake), "wake selected PI mutex waiter");
        }
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
    fn new_owner_registration_disables_preemption_until_scheduler_publication() {
        let (system, cpu) = install_current_thread();
        let _runtime = crate::test_runtime::install(
            (&*system as *const TaskSystem).expose_provenance(),
            (cpu.as_ref().get_ref() as *const ax_task::CpuLocal).expose_provenance(),
        );
        let raw = RawMutex::new();
        let current = current_thread("test new PI owner registration");

        let attempt = raw.try_or_observe_owner(&current);

        assert!(matches!(attempt, LockAttempt::Acquired(_)));
        assert_eq!(crate::test_runtime::preempt_depth(), 1);
        drop(attempt);
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
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
        system
            .pi_mutex_acquired(raw.lock_id(), current.id())
            .unwrap();
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
    fn uncontended_lock_registers_and_releases_pi_ownership() {
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
        system.pi_mutex_acquired(raw.lock_id(), owner.id()).unwrap();
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
