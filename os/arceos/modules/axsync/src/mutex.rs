//! Priority-inheritance sleeping mutex.

use core::{
    pin::pin,
    sync::atomic::{AtomicU64, Ordering},
};

use ax_kernel_guard::NoPreempt as PreemptGuard;
use ax_kspin::SpinNoIrq;
use ax_task::{
    PiLockId, PiLockIdentity, PiWaitToken, TaskError, ThreadHandle, ThreadId,
    current_thread_handle, current_thread_id, pi_block_current, pi_wait_start, pi_wake,
    prepare_pi_mutex_handoff, validate_blocking_context,
};

use crate::pi::{WaiterNode, WaiterPointer, WaiterQueue};

/// A non-recursive, urgency-ordered PI mutex implementing `lock_api::RawMutex`.
///
/// Owner, waiter-list, donation registration, and handoff are serialized by
/// [`SpinNoIrq`]. Blocking and targeted wake happen after that metadata guard
/// has been released.
pub struct RawMutex {
    metadata: SpinNoIrq<MutexMetadata>,
    identity: PiLockIdentity,
    next_waiter_sequence: AtomicU64,
    #[cfg(feature = "lockdep")]
    pub(crate) lockdep: crate::lockdep::LockdepMap,
}

#[derive(Debug)]
struct MutexMetadata {
    owner: Option<ThreadId>,
    waiters: WaiterQueue,
}

enum LockAttempt {
    Acquired,
    Contended,
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
            identity: PiLockIdentity::new(),
            next_waiter_sequence: AtomicU64::new(0),
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
        task_result(self.identity.id(), "allocate PI mutex identity")
    }

    fn lock_pi(&self) {
        let current = current_thread_identity("lock PI mutex");
        let mut blocking_context_validated = false;

        loop {
            match self.try_or_observe_owner(current) {
                LockAttempt::Acquired => return,
                LockAttempt::Contended => {
                    if !blocking_context_validated {
                        // The uncontended path neither publishes a waiter nor
                        // schedules and must remain usable during
                        // single-threaded boot.
                        task_result(
                            validate_blocking_context(),
                            "validate PI mutex sleep context",
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

    fn lock_contended(&self, current: ThreadId) {
        let current_handle = current_thread("register PI mutex waiter");
        assert_eq!(
            current_handle.id(),
            current,
            "PI mutex contender identity changed while preemption was disabled"
        );
        let sequence = self.next_waiter_sequence.fetch_add(1, Ordering::Relaxed);
        let waiter = pin!(WaiterNode::new(
            current,
            current_handle.effective_scheduling_urgency(),
            sequence,
            current_handle.clone(),
        ));
        let token = {
            let mut metadata = self.metadata.lock();
            let owner = match metadata.owner {
                None => {
                    metadata.owner = Some(current);
                    return;
                }
                Some(owner) if owner == current => {
                    panic!("thread attempted recursive PI mutex acquisition")
                }
                Some(owner) => owner,
            };
            let token = task_result(pi_wait_start(self.lock_id(), owner), "start PI mutex wait");
            // SAFETY: the scheduler donation and local queue publication share
            // this metadata transaction, and the lock call keeps the stack
            // node pinned until handoff removes it.
            unsafe { metadata.waiters.insert(waiter.as_ref()) };
            token
        };
        self.wait_for_handoff(&waiter, token);
    }

    fn wait_for_handoff(&self, waiter: &core::pin::Pin<&mut WaiterNode>, token: PiWaitToken) {
        if !token.is_granted() {
            task_result(pi_block_current(&token), "block on PI mutex");
        }
        assert!(
            waiter.is_granted(),
            "scheduler PI grant became visible before local mutex ownership"
        );
    }

    fn try_or_observe_owner(&self, current: ThreadId) -> LockAttempt {
        let mut metadata = self.metadata.lock();
        match metadata.owner {
            None => {
                metadata.owner = Some(current);
                LockAttempt::Acquired
            }
            Some(owner) if owner == current => {
                panic!("thread attempted recursive PI mutex acquisition")
            }
            Some(_) => LockAttempt::Contended,
        }
    }

    fn try_lock_pi(&self) -> bool {
        let current = current_thread_identity("try PI mutex");
        let mut metadata = self.metadata.lock();
        if metadata.owner.is_some() {
            return false;
        }
        metadata.owner = Some(current);
        true
    }

    fn unlock_pi(&self) {
        let current = current_thread_identity("unlock PI mutex");
        // Linux retains preemption exclusion from owner deboost through the
        // deferred wake. This prevents an unrelated task from running between
        // lowering the old owner and making its top donor runnable.
        let _preempt_guard = PreemptGuard::new();
        let wake = {
            let mut metadata = self.metadata.lock();
            assert_eq!(
                metadata.owner,
                Some(current),
                "thread attempted to unlock a PI mutex it does not own"
            );
            let selected = select_most_urgent_waiter(metadata.waiters.head());
            let Some(selected) = selected else {
                metadata.owner = None;
                return;
            };
            let next_owner = unsafe {
                // SAFETY: the selected waiter remains pinned until this
                // transaction publishes both local and scheduler grant.
                selected.thread_id()
            };
            let scheduler_handoff = task_result(
                prepare_pi_mutex_handoff(self.lock_id(), current, Some(next_owner)),
                "prepare PI mutex handoff",
            );
            let selected = metadata
                .waiters
                .remove(&selected)
                .expect("selected PI waiter must remain queued");
            metadata.owner = Some(next_owner);
            let wake = unsafe {
                // SAFETY: local ownership must precede scheduler token grant,
                // and this pinned node cannot leave its lock call until it
                // observes the grant.
                selected.grant();
                selected.wake_handle()
            }
            .expect("production PI waiters always carry a wake handle");
            // SAFETY: metadata still owns the local transaction, `next_owner`
            // and the local waiter grant are published above, and the selected
            // waiter remains pinned until the deferred wake is observed.
            unsafe { scheduler_handoff.commit_after_local_handoff() };
            wake
        };
        task_result(pi_wake(&wake), "wake selected PI mutex waiter");
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

impl MutexMetadata {
    const fn new() -> Self {
        Self {
            owner: None,
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
        #[cfg(feature = "lockdep")]
        crate::lockdep::release(self);
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
    let mut selected: Option<(WaiterPointer, (ax_task::SchedulingUrgency, u64))> = None;
    while let Some(waiter) = current {
        // SAFETY: the metadata lock prevents enqueue/removal and every blocked
        // waiter remains pinned until handoff publishes grant.
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

/// A safe PI mutex using [`RawMutex`].
pub type Mutex<T> = lock_api::Mutex<RawMutex, T>;
/// A non-send guard returned by [`Mutex`].
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, RawMutex, T>;

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::{mem::MaybeUninit, pin::Pin};

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
        assert!(metadata.waiters.is_empty());
    }

    #[test]
    fn reconstructed_mutex_does_not_reuse_the_previous_pi_identity() {
        let mut storage = MaybeUninit::<RawMutex>::uninit();
        let pointer = storage.as_mut_ptr();

        let (first, second) = unsafe {
            // SAFETY: each write begins a fresh RawMutex lifetime in the same
            // storage, and each initialized value is dropped exactly once.
            pointer.write(RawMutex::new());
            let first = (&*pointer).lock_id();
            pointer.drop_in_place();
            pointer.write(RawMutex::new());
            let second = (&*pointer).lock_id();
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

        assert_eq!(raw.lock_id(), raw.lock_id());
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
        drop(metadata);
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
            waiter_thread.effective_scheduling_urgency(),
            0,
            waiter_thread.clone(),
        ));
        {
            let mut metadata = raw.metadata.lock();
            metadata.owner = Some(owner.id());
            // SAFETY: this pinned waiter remains live until the test removes it
            // through the handoff below.
            unsafe { metadata.waiters.insert(waiter.as_ref()) };
        }

        let preempt_guard = PreemptGuard::new();
        let mut metadata = raw.metadata.lock();
        let selected = select_most_urgent_waiter(metadata.waiters.head()).unwrap();
        let handoff = system
            .prepare_pi_mutex_handoff(raw.lock_id(), owner.id(), Some(waiter_thread.id()))
            .unwrap();
        let selected = metadata.waiters.remove(&selected).unwrap();
        metadata.owner = Some(waiter_thread.id());
        // SAFETY: this pinned test waiter remains live through the complete
        // scheduler transaction.
        unsafe { selected.grant() };
        assert!(waiter.is_granted());
        assert!(!token.is_granted());
        assert!(crate::test_runtime::preempt_depth() > 0);
        // SAFETY: local owner and waiter grant were published above while both
        // the mutex metadata and scheduler transaction remain locked.
        unsafe { handoff.commit_after_local_handoff() };
        drop(metadata);
        drop(preempt_guard);

        assert!(token.is_granted());
        assert!(waiter.is_granted());
        assert_eq!(raw.metadata.lock().owner, Some(waiter_thread.id()));
        assert_eq!(crate::test_runtime::preempt_depth(), 0);
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
        }
        crate::test_runtime::set_schedule_context_safe(false);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lock_api::RawMutex::lock(&raw);
        }));

        assert!(result.is_err());
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
