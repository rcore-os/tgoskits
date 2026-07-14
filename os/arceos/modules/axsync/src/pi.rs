//! Priority-inheritance mutex metadata.

use core::{
    cell::UnsafeCell,
    marker::PhantomPinned,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_task::{SchedulingKey, ThreadHandle, ThreadId, ThreadWakeHandle};

/// A pinned waiter embedded in the blocked `RawMutex::lock` call frame.
pub(crate) struct WaiterNode {
    thread_id: ThreadId,
    urgency: SchedulingKey,
    sequence: u64,
    granted: AtomicBool,
    thread: Option<ThreadHandle>,
    next: UnsafeCell<Option<NonNull<WaiterNode>>>,
    _pinned: PhantomPinned,
}

/// Intrusive waiter list sorted from most to least urgent.
#[derive(Debug)]
pub(crate) struct WaiterQueue {
    head: Option<NonNull<WaiterNode>>,
    len: usize,
}

/// Pointer removed from a [`WaiterQueue`] while the waiter remains pinned.
pub(crate) struct WaiterPointer(NonNull<WaiterNode>);

impl WaiterNode {
    /// Creates an unlinked waiter owned by the current lock call frame.
    pub(crate) const fn new(
        thread_id: ThreadId,
        urgency: SchedulingKey,
        sequence: u64,
        thread: ThreadHandle,
    ) -> Self {
        Self {
            thread_id,
            urgency,
            sequence,
            granted: AtomicBool::new(false),
            thread: Some(thread),
            next: UnsafeCell::new(None),
            _pinned: PhantomPinned,
        }
    }

    /// Returns whether unlock has transferred ownership to this waiter.
    pub(crate) fn is_granted(&self) -> bool {
        self.granted.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn new_for_test(
        thread_id: ThreadId,
        urgency: SchedulingKey,
        sequence: u64,
    ) -> Pin<alloc::boxed::Box<Self>> {
        alloc::boxed::Box::pin(Self {
            thread_id,
            urgency,
            sequence,
            granted: AtomicBool::new(false),
            thread: None,
            next: UnsafeCell::new(None),
            _pinned: PhantomPinned,
        })
    }

    #[inline(always)]
    fn ordering_key(&self) -> (SchedulingKey, u64) {
        (self.urgency, self.sequence)
    }

    /// Publishes ownership before the selected waiter is woken.
    fn grant(&self) {
        self.granted.store(true, Ordering::Release);
    }

    /// Reads the intrusive link while the mutex metadata lock is held.
    ///
    /// # Safety
    ///
    /// The caller must hold the metadata lock that exclusively owns the list.
    unsafe fn next(&self) -> Option<NonNull<Self>> {
        // SAFETY: required by this method's contract.
        unsafe { *self.next.get() }
    }

    /// Updates the intrusive link while the mutex metadata lock is held.
    ///
    /// # Safety
    ///
    /// The caller must hold the metadata lock that exclusively owns the list.
    unsafe fn set_next(&self, next: Option<NonNull<Self>>) {
        // SAFETY: required by this method's contract.
        unsafe { *self.next.get() = next };
    }
}

impl WaiterQueue {
    /// Creates an empty waiter queue suitable for static mutex initialization.
    pub(crate) const fn new() -> Self {
        Self { head: None, len: 0 }
    }

    /// Inserts one pinned waiter according to effective scheduler urgency.
    ///
    /// # Safety
    ///
    /// `waiter` must remain pinned and alive until it is removed from this
    /// queue. The caller must hold the mutex metadata lock.
    pub(crate) unsafe fn insert(&mut self, waiter: Pin<&WaiterNode>) {
        let waiter_ptr = NonNull::from(waiter.get_ref());
        let mut previous: Option<NonNull<WaiterNode>> = None;
        let mut current = self.head;

        while let Some(current_ptr) = current {
            // SAFETY: every queued pointer satisfies `insert`'s lifetime
            // contract, and the metadata lock prevents list mutation races.
            let current_ref = unsafe { current_ptr.as_ref() };
            if waiter.ordering_key() < current_ref.ordering_key() {
                break;
            }
            previous = current;
            // SAFETY: the metadata lock is held.
            current = unsafe { current_ref.next() };
        }

        // SAFETY: the metadata lock is held and waiter is not linked yet.
        unsafe { waiter.set_next(current) };
        if let Some(previous_ptr) = previous {
            // SAFETY: previous is a live queued node and metadata is locked.
            unsafe { previous_ptr.as_ref().set_next(Some(waiter_ptr)) };
        } else {
            self.head = Some(waiter_ptr);
        }
        self.len += 1;
    }

    /// Removes and returns the most urgent waiter.
    ///
    /// The caller must hold the mutex metadata lock.
    #[cfg(test)]
    pub(crate) fn pop_front(&mut self) -> Option<WaiterPointer> {
        let head = self.head?;
        // SAFETY: queue pointers remain live until removal and metadata is held.
        let head_ref = unsafe { head.as_ref() };
        // SAFETY: the metadata lock is held.
        self.head = unsafe { head_ref.next() };
        // SAFETY: the removed node is no longer part of this list.
        unsafe { head_ref.set_next(None) };
        self.len -= 1;
        Some(WaiterPointer(head))
    }

    /// Returns the current head while the owner transition freezes insertion.
    pub(crate) fn head(&self) -> Option<WaiterPointer> {
        self.head.map(WaiterPointer)
    }

    /// Removes a previously selected waiter.
    ///
    /// The caller must hold the metadata lock and must have frozen insertion by
    /// clearing `owner_registered` before selecting this pointer.
    pub(crate) fn remove(&mut self, selected: &WaiterPointer) -> Option<WaiterPointer> {
        let mut previous: Option<NonNull<WaiterNode>> = None;
        let mut current = self.head;
        while let Some(current_ptr) = current {
            // SAFETY: insertion is frozen and all queued nodes stay pinned.
            let current_ref = unsafe { current_ptr.as_ref() };
            // SAFETY: the metadata lock is held.
            let next = unsafe { current_ref.next() };
            if current_ptr == selected.0 {
                if let Some(previous_ptr) = previous {
                    // SAFETY: previous is a live queued node under metadata lock.
                    unsafe { previous_ptr.as_ref().set_next(next) };
                } else {
                    self.head = next;
                }
                // SAFETY: selected is no longer part of this list.
                unsafe { current_ref.set_next(None) };
                self.len -= 1;
                return Some(WaiterPointer(current_ptr));
            }
            previous = current;
            current = next;
        }
        None
    }

    /// Returns whether the queue contains no waiters.
    #[cfg(test)]
    pub(crate) const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl WaiterPointer {
    /// Returns the selected waiter's thread identity.
    /// # Safety
    ///
    /// The waiter must still be pinned in its lock call frame.
    pub(crate) unsafe fn thread_id(&self) -> ThreadId {
        // SAFETY: forwarded caller contract keeps the waiter alive.
        unsafe { self.node() }.thread_id
    }

    /// Publishes ownership transfer to the waiter.
    ///
    /// # Safety
    ///
    /// The waiter must still be pinned in its lock call frame.
    pub(crate) unsafe fn grant(&self) {
        // SAFETY: forwarded caller contract keeps the waiter alive.
        unsafe { self.node() }.grant();
    }

    /// Clones the direct targeted-wake handle in task context.
    ///
    /// # Safety
    ///
    /// The waiter must still be pinned in its lock call frame.
    pub(crate) unsafe fn wake_handle(&self) -> Option<ThreadWakeHandle> {
        // SAFETY: forwarded caller contract keeps the waiter alive.
        unsafe { self.node() }
            .thread
            .as_ref()
            .map(ThreadHandle::wake_handle)
    }

    /// Returns this waiter's latest effective ordering key.
    ///
    /// # Safety
    ///
    /// The waiter must remain pinned, and enqueue/removal must be frozen while
    /// the key is sampled outside the metadata lock.
    pub(crate) unsafe fn effective_ordering_key(&self) -> (SchedulingKey, u64) {
        // SAFETY: forwarded caller contract keeps the waiter alive.
        let node = unsafe { self.node() };
        let urgency = node
            .thread
            .as_ref()
            .map(ThreadHandle::effective_scheduling_key)
            .unwrap_or(node.urgency);
        (urgency, node.sequence)
    }

    /// Advances through the frozen intrusive list.
    ///
    /// # Safety
    ///
    /// The waiter list must be frozen and every node must remain pinned.
    pub(crate) unsafe fn next(&self) -> Option<Self> {
        // SAFETY: forwarded caller contract keeps the waiter and list stable.
        unsafe { self.node().next() }.map(Self)
    }

    unsafe fn node(&self) -> &WaiterNode {
        // SAFETY: required by this method's caller contract.
        unsafe { self.0.as_ref() }
    }
}

// SAFETY: list pointers are accessed only while the owning mutex's SpinNoIrq
// metadata lock is held. Nodes remain pinned until they are removed and granted.
unsafe impl Send for WaiterQueue {}

// SAFETY: `granted` is atomic; `next` is touched only under the metadata lock;
// all other fields are immutable while the waiter is published.
unsafe impl Sync for WaiterNode {}

#[cfg(test)]
mod tests {
    use ax_task::{
        CpuId, FairMode, Nice, PiLockId, RtPriority, SchedulePolicy, SchedulingKey, TaskSystem,
        TaskSystemConfig, ThreadHandle, ThreadId, ThreadSpec,
    };

    use super::*;

    #[test]
    fn pops_waiters_in_effective_urgency_order() {
        let mut queue = WaiterQueue::new();
        let fair = WaiterNode::new_for_test(thread(1), key(2, 100), 0);
        let rt = WaiterNode::new_for_test(thread(2), key(1, 80), 1);
        let deadline = WaiterNode::new_for_test(thread(3), key(0, 50), 2);

        unsafe {
            queue.insert(fair.as_ref());
            queue.insert(rt.as_ref());
            queue.insert(deadline.as_ref());
        }

        unsafe {
            assert_eq!(queue.pop_front().unwrap().thread_id(), thread(3));
            assert_eq!(queue.pop_front().unwrap().thread_id(), thread(2));
            assert_eq!(queue.pop_front().unwrap().thread_id(), thread(1));
        }
    }

    #[test]
    fn preserves_fifo_order_for_equal_urgency() {
        let mut queue = WaiterQueue::new();
        let first = WaiterNode::new_for_test(thread(1), key(1, 50), 1);
        let second = WaiterNode::new_for_test(thread(2), key(1, 50), 2);

        unsafe {
            queue.insert(first.as_ref());
            queue.insert(second.as_ref());
        }

        unsafe {
            assert_eq!(queue.pop_front().unwrap().thread_id(), thread(1));
            assert_eq!(queue.pop_front().unwrap().thread_id(), thread(2));
        }
    }

    #[test]
    fn grant_is_visible_before_targeted_wake() {
        let waiter = WaiterNode::new_for_test(thread(1), key(2, 0), 0);

        assert!(!waiter.is_granted());
        waiter.grant();
        assert!(waiter.is_granted());
    }

    #[test]
    fn releasing_one_owned_lock_preserves_other_lock_donation() {
        let system = task_system(1);
        let owner = create_thread(&system, fair_policy());
        let low_donor = create_thread(&system, fifo_policy(20));
        let high_donor = create_thread(&system, fifo_policy(80));
        let low_lock = PiLockId::new(1);
        let high_lock = PiLockId::new(2);

        let low_wait = system
            .pi_wait_start(low_lock, low_donor.id(), owner.id())
            .unwrap();
        assert_effective(&owner, fifo_policy(20));
        let high_wait = system
            .pi_wait_start(high_lock, high_donor.id(), owner.id())
            .unwrap();
        assert_effective(&owner, fifo_policy(80));

        system
            .pi_mutex_handoff(high_lock, owner.id(), Some(high_donor.id()))
            .unwrap();
        assert!(high_wait.is_granted());
        assert_effective(&owner, fifo_policy(20));

        system
            .pi_mutex_handoff(low_lock, owner.id(), Some(low_donor.id()))
            .unwrap();
        assert!(low_wait.is_granted());
        assert_effective(&owner, fair_policy());
    }

    #[test]
    fn transitive_donation_propagates_and_withdraws_along_wait_chain() {
        let system = task_system(1);
        let first_owner = create_thread(&system, fair_policy());
        let second_owner = create_thread(&system, fifo_policy(30));
        let final_donor = create_thread(&system, fifo_policy(90));
        let first_lock = PiLockId::new(11);
        let second_lock = PiLockId::new(12);

        let middle_wait = system
            .pi_wait_start(first_lock, second_owner.id(), first_owner.id())
            .unwrap();
        assert_effective(&first_owner, fifo_policy(30));
        let final_wait = system
            .pi_wait_start(second_lock, final_donor.id(), second_owner.id())
            .unwrap();
        assert_effective(&second_owner, fifo_policy(90));
        assert_effective(&first_owner, fifo_policy(90));

        system.pi_wait_cancel(final_wait).unwrap();
        assert_effective(&second_owner, fifo_policy(30));
        assert_effective(&first_owner, fifo_policy(30));
        system.pi_wait_cancel(middle_wait).unwrap();
        assert_effective(&first_owner, fair_policy());
    }

    #[test]
    fn queued_remote_owner_boost_requests_its_cpu_reschedule() {
        let system = task_system(2);
        let mut remote_cpu = system.create_cpu_local(CpuId::new(1)).unwrap();
        system.bring_cpu_online(remote_cpu.as_mut()).unwrap();
        let owner = create_thread(&system, fair_policy());
        system.make_ready(owner.id()).unwrap();
        system.enqueue(remote_cpu.as_mut(), owner.id(), 0).unwrap();
        let donor = create_thread(&system, fifo_policy(70));
        let lock = PiLockId::new(21);
        crate::test_runtime::reset_scheduler_ipis();

        let _wait = system.pi_wait_start(lock, donor.id(), owner.id()).unwrap();

        assert_effective(&owner, fifo_policy(70));
        assert_eq!(crate::test_runtime::scheduler_ipi_count(), 1);
        assert_eq!(crate::test_runtime::last_scheduler_ipi_cpu(), Some(1));
        let drained = system.drain_policy_updates(remote_cpu.as_mut(), 0).unwrap();
        assert_eq!(drained.drained(), 1);
        assert!(!drained.pending());
    }

    #[test]
    #[should_panic(expected = "scheduler invariant reported by ax-sync unit test")]
    fn donation_cycle_is_a_fatal_scheduler_invariant() {
        let system = task_system(1);
        let first = create_thread(&system, fair_policy());
        let second = create_thread(&system, fair_policy());
        let first_lock = PiLockId::new(31);
        let second_lock = PiLockId::new(32);
        let _first_wait = system
            .pi_wait_start(first_lock, second.id(), first.id())
            .unwrap();

        let _cycle = system
            .pi_wait_start(second_lock, first.id(), second.id())
            .unwrap();
    }

    fn thread(slot: u32) -> ThreadId {
        ThreadId::from_parts(slot, 1)
    }

    fn key(class_rank: u8, primary: u64) -> SchedulingKey {
        SchedulingKey::new(class_rank, primary, 0)
    }

    fn task_system(cpu_count: usize) -> TaskSystem {
        TaskSystem::new(TaskSystemConfig::new(cpu_count)).unwrap()
    }

    fn create_thread(system: &TaskSystem, policy: SchedulePolicy) -> ThreadHandle {
        system.create_thread(ThreadSpec::new(policy)).unwrap()
    }

    fn fair_policy() -> SchedulePolicy {
        SchedulePolicy::fair(Nice::ZERO, FairMode::Normal)
    }

    fn fifo_policy(priority: u8) -> SchedulePolicy {
        SchedulePolicy::fifo(RtPriority::new(priority).unwrap())
    }

    fn assert_effective(thread: &ThreadHandle, policy: SchedulePolicy) {
        assert_eq!(
            thread.effective_scheduling_key(),
            policy.scheduling_key(thread.id().as_u64())
        );
    }
}
