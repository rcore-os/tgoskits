//! Mutex waiter-tree ownership and bounded PI graph propagation.

use super::*;

impl TaskSystem {
    /// Replaces the one waiter-tree top contributed by a physical mutex.
    ///
    /// The caller owns that mutex's wait lock. This method then acquires only
    /// the owner task's PI lock, exactly matching Linux's wait-lock -> pi-lock
    /// order for `rt_mutex_enqueue_pi()`/`rt_mutex_dequeue_pi()`.
    pub(in crate::system::task_system) fn replace_owner_lock_top(
        &self,
        owner: ThreadId,
        old_top: Option<(PiWaitKey, PiDonation)>,
        new_top: Option<(PiWaitKey, PiDonation)>,
    ) -> Result<(), TaskError> {
        let owner_core = self.pi_thread_core(owner)?;
        let old_core = old_top
            .as_ref()
            .map(|(_, donation)| donation.waiter_core().ok_or(TaskError::InvalidPiState))
            .transpose()?;
        let new_core = new_top
            .as_ref()
            .map(|(_, donation)| donation.waiter_core().ok_or(TaskError::InvalidPiState))
            .transpose()?;
        let mut owner_sched = owner_core.sched().lock();
        if owner_sched.lifecycle.state() == ThreadState::Exited {
            return Err(TaskError::InvalidPiWaitState(
                PiWaitStateError::ExitedParticipant,
            ));
        }
        if let (Some((old_top, _)), Some((new_top, donation))) =
            (old_top.as_ref(), new_top.as_ref())
            && *old_top == *new_top
            && owner_sched
                .pi
                .donors
                .donation(*old_top)
                .is_some_and(|current| current.same_source(donation))
        {
            return Ok(());
        }
        // Linux validates the complete rt_mutex_setprio() transaction before
        // changing p->pi_waiters. A generation overflow is a typed policy
        // failure, not a reason to strand the physical waiter tree on a
        // donor which the owner rq could not publish.
        owner_sched
            .policy
            .dispatch_generation
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        let old_key = old_top.as_ref().map(|(key, _)| *key);
        let remaining_top = owner_sched.pi.donors.first_entry_excluding(old_key);
        let prospective_top = match (remaining_top, new_top.as_ref()) {
            (Some(current), Some(candidate)) if candidate.0 < current.0 => Some(candidate.clone()),
            (Some(current), _) => Some(current),
            (None, Some(candidate)) => Some(candidate.clone()),
            (None, None) => None,
        };
        if let Some((old_top, _)) = old_top.as_ref() {
            let removed = owner_sched
                .pi
                .donors
                .remove(*old_top)
                .ok_or(TaskError::InvalidPiState)?;
            // SAFETY: the mutex wait lock and owner PI lock detached the only
            // owner-tree linkage which can use this preallocated node.
            unsafe {
                old_core
                    .as_ref()
                    .expect("old PI top must retain its task")
                    .pi_wait_nodes()
                    .return_owner_donor(removed)
            };
        }
        if let Some((new_top, donation)) = new_top.as_ref() {
            let new_core = new_core.as_ref().expect("new PI top must retain its task");
            let inserted = unsafe {
                // SAFETY: one blocked waiter is the top waiter of at most one
                // mutex and therefore can own one owner-tree linkage.
                new_core.pi_wait_nodes().take_owner_donor()
            };
            owner_sched
                .pi
                .donors
                .insert(*new_top, donation.clone(), inserted);
        }
        if let Err(error) =
            self.recompute_pi_owner_locked(&owner_core, &mut owner_sched, prospective_top)
        {
            if let Some((new_top, _)) = new_top.as_ref() {
                let removed = owner_sched
                    .pi
                    .donors
                    .remove(*new_top)
                    .expect("failed PI owner update must retain the proposed donor");
                // SAFETY: the failed transaction removed the only owner-tree
                // linkage before returning it to the waiter's storage.
                unsafe {
                    new_core
                        .as_ref()
                        .expect("new PI top must retain its task")
                        .pi_wait_nodes()
                        .return_owner_donor(removed)
                };
            }
            if let Some((old_top, donation)) = old_top.as_ref() {
                let old_core = old_core.as_ref().expect("old PI top must retain its task");
                let restored = unsafe {
                    // SAFETY: the failed transaction returned this task's
                    // owner linkage above and no other owner can consume it
                    // while the physical mutex wait lock is held.
                    old_core.pi_wait_nodes().take_owner_donor()
                };
                owner_sched
                    .pi
                    .donors
                    .insert(*old_top, donation.clone(), restored);
            }
            return Err(error);
        }
        Ok(())
    }

    /// Publishes a physical mutex's cached-top change while its wait lock is held.
    fn publish_lock_top_change(
        &self,
        owner: Option<ThreadId>,
        old_top: Option<(PiWaitKey, PiDonation)>,
        new_top: Option<(PiWaitKey, PiDonation)>,
        rekey: Option<PiWaiterRekey>,
    ) -> Result<(), TaskError> {
        let old_key = old_top.as_ref().map(|(key, _)| *key);
        let new_key = new_top.as_ref().map(|(key, _)| *key);
        let marker = |top: &Option<(PiWaitKey, PiDonation)>| {
            let Some((key, donation)) = top.as_ref() else {
                return Ok(None);
            };
            let core = donation.waiter_core().ok_or(TaskError::InvalidPiState)?;
            if let Some(rekey) = rekey
                && core.id() == rekey.waiter
                && (*key == rekey.registration.key || *key == rekey.new_key)
            {
                return Ok(Some((core, rekey.registration.generation)));
            }
            let registration = core
                .sched()
                .lock()
                .pi
                .blocked_on
                .ok_or(TaskError::InvalidPiState)?;
            if registration.key != *key {
                return Err(TaskError::InvalidPiState);
            }
            Ok(Some((core, registration.generation)))
        };
        let (old_marker, new_marker) = if old_key != new_key {
            let old_marker = marker(&old_top)?;
            let new_marker = marker(&new_top)?;
            if let Some((core, generation)) = new_marker.as_ref()
                && !core.pi_wait_state().can_grant(*generation)
            {
                return Err(TaskError::InvalidPiState);
            }
            (old_marker, new_marker)
        } else {
            (None, None)
        };
        if let Some(owner) = owner {
            self.replace_owner_lock_top(owner, old_top, new_top)?;
        }
        if let Some((core, generation)) = old_marker {
            core.pi_wait_state().clear_top(generation);
        }
        if let Some((core, generation)) = new_marker {
            core.pi_wait_state()
                .mark_top(generation)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5049_1204, core.id().as_u64() as usize)
                });
        }
        Ok(())
    }

    pub(in crate::system::task_system) fn insert_lock_waiter(
        &self,
        lock_state: &mut PiMutexWaiters,
        owner: Option<ThreadId>,
        waiter_core: &Arc<ThreadCore>,
        registration: PiWaitRegistration,
        donation: PiDonation,
    ) -> Result<(), TaskError> {
        let old_top = lock_state.waiters.first_entry();
        {
            let mut waiter_sched = waiter_core.sched().lock();
            if waiter_sched.pi.blocked_on.is_some() || lock_state.waiters.contains(registration.key)
            {
                return Err(TaskError::InvalidPiState);
            }
            let node = unsafe {
                // SAFETY: `blocked_on == None` proves this task cannot already
                // own a physical-lock waiter linkage.
                waiter_core.pi_wait_nodes().take_lock_waiter()
            };
            lock_state.waiters.insert(registration.key, donation, node);
            waiter_sched.pi.blocked_on = Some(registration);
        }
        if let Err(error) =
            self.publish_lock_top_change(owner, old_top, lock_state.waiters.first_entry(), None)
        {
            let removed = lock_state
                .waiters
                .remove(registration.key)
                .ok_or(TaskError::InvalidPiState)?;
            let mut waiter_sched = waiter_core.sched().lock();
            if waiter_sched.pi.blocked_on != Some(registration) {
                return Err(TaskError::InvalidPiState);
            }
            waiter_sched.pi.blocked_on = None;
            drop(waiter_sched);
            // SAFETY: wait_lock detached the failed insertion and the task PI
            // lock removed its only registration before storage is returned.
            unsafe { waiter_core.pi_wait_nodes().return_lock_waiter(removed) };
            return Err(error);
        }
        Ok(())
    }

    pub(in crate::system::task_system) fn remove_lock_waiter(
        &self,
        lock_state: &mut PiMutexWaiters,
        owner: Option<ThreadId>,
        waiter_core: &Arc<ThreadCore>,
        generation: u64,
    ) -> Result<PiWaitRegistration, TaskError> {
        let old_top = lock_state.waiters.first_entry();
        let (registration, removed, donation) = {
            let waiter_sched = waiter_core.sched().lock();
            let registration = waiter_sched
                .pi
                .blocked_on
                .filter(|registration| registration.generation == generation)
                .ok_or(TaskError::InvalidPiState)?;
            let donation = lock_state
                .waiters
                .donation(registration.key)
                .ok_or(TaskError::InvalidPiState)?;
            let removed = lock_state
                .waiters
                .remove(registration.key)
                .ok_or(TaskError::InvalidPiState)?;
            (registration, removed, donation)
        };
        if let Err(error) = self.publish_lock_top_change(
            owner,
            old_top.clone(),
            lock_state.waiters.first_entry(),
            None,
        ) {
            lock_state
                .waiters
                .insert(registration.key, donation, removed);
            return Err(error);
        }
        {
            let mut waiter_sched = waiter_core.sched().lock();
            if waiter_sched.pi.blocked_on != Some(registration) {
                task_runtime::fatal_invariant(0x5049_1203, waiter_core.id().as_u64() as usize);
            }
            waiter_sched.pi.blocked_on = None;
        }
        // SAFETY: the mutex wait lock detached this node and the task PI lock
        // cleared the only registration which could name it. Keeping
        // `blocked_on` installed through `publish_lock_top_change()` matches
        // Linux's wait_lock + pi_lock transaction: owner-top propagation may
        // still inspect the waiter until the tree update is globally complete.
        unsafe { waiter_core.pi_wait_nodes().return_lock_waiter(removed) };
        waiter_core
            .pi_wait_state()
            .clear_top(registration.generation);
        Ok(registration)
    }

    /// Requeues one blocked waiter after its effective urgency changed.
    ///
    /// The normal order is mutex wait lock then task PI lock. Chain propagation
    /// begins from a task PI lock, so it mirrors Linux step [5]: try the wait
    /// lock, drop/retry on contention, and revalidate `blocked_on` after success.
    fn refresh_blocked_waiter_key(
        &self,
        waiter_core: &Arc<ThreadCore>,
    ) -> Result<PiWaiterRefresh, TaskError> {
        loop {
            let donation = self.pi_donation(waiter_core)?;
            let mut waiter_sched = waiter_core.sched().lock();
            let Some(registration) = waiter_sched.pi.blocked_on else {
                return Ok(PiWaiterRefresh {
                    owner: None,
                    changed: false,
                });
            };
            let urgency = waiter_core.effective_pi_wait_urgency();
            let Some(mut lock_state) = (unsafe {
                // SAFETY: `blocked_on` pins the mutex identity until this
                // registration is removed under the same task PI lock.
                registration.lock.try_lock_state()
            }) else {
                drop(waiter_sched);
                core::hint::spin_loop();
                continue;
            };
            if waiter_sched.pi.blocked_on != Some(registration) {
                continue;
            }
            let old_donation = lock_state
                .waiters
                .donation(registration.key)
                .ok_or(TaskError::InvalidPiState)?;
            if urgency == registration.key.urgency && donation.same_source(&old_donation) {
                return Ok(PiWaiterRefresh {
                    owner: unsafe { registration.lock.core() }.owner_snapshot().owner(),
                    changed: false,
                });
            }
            let old_top = lock_state.waiters.first_entry();
            let node = lock_state
                .waiters
                .remove(registration.key)
                .ok_or(TaskError::InvalidPiState)?;
            let new_key = PiWaitKey::new(urgency, registration.key.sequence, waiter_core.id());
            lock_state.waiters.insert(new_key, donation, node);
            let owner = unsafe { registration.lock.core() }.owner_snapshot().owner();
            if let Err(error) = self.publish_lock_top_change(
                owner,
                old_top.clone(),
                lock_state.waiters.first_entry(),
                Some(PiWaiterRekey {
                    waiter: waiter_core.id(),
                    registration,
                    new_key,
                }),
            ) {
                let node = lock_state
                    .waiters
                    .remove(new_key)
                    .ok_or(TaskError::InvalidPiState)?;
                lock_state
                    .waiters
                    .insert(registration.key, old_donation, node);
                return Err(error);
            }
            let current = waiter_sched.pi.blocked_on.as_mut().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5049_1210, waiter_core.id().as_u64() as usize)
            });
            if *current != registration {
                task_runtime::fatal_invariant(0x5049_1205, waiter_core.id().as_u64() as usize);
            }
            current.key = new_key;
            return Ok(PiWaiterRefresh {
                owner,
                changed: true,
            });
        }
    }

    /// Propagates one already committed PI owner update through blocked owners.
    ///
    /// No invocation owns more than one task PI lock plus one mutex wait lock.
    /// `origin_lock` enables Linux's full chain walk after a new edge is
    /// installed so a concurrent indirect cycle is detected and rolled back.
    pub(in crate::system::task_system) fn recompute_pi_chain(
        &self,
        start: ThreadId,
        origin_lock: Option<PiMutexRaw>,
        top_task: ThreadId,
    ) -> Result<(), TaskError> {
        self.recompute_pi_chain_bounded(start, origin_lock, top_task, self.config.pi_chain_limit())
    }

    /// Propagates a committed PI removal or priority change through the
    /// existing dependency graph.
    ///
    /// Linux uses the minimum chain-walk mode for these adjustments: the
    /// configured admission bound applies to adding a new dependency, not to
    /// restoring an already accepted graph after unlock, cancellation, or a
    /// policy change. The fixed thread capacity is the structural upper bound
    /// of an acyclic in-kernel wait graph.
    pub(in crate::system::task_system) fn recompute_pi_cleanup_chain(
        &self,
        start: ThreadId,
        top_task: ThreadId,
    ) -> Result<(), TaskError> {
        self.recompute_pi_chain_bounded(start, None, top_task, self.config.thread_capacity())
    }

    fn recompute_pi_chain_bounded(
        &self,
        start: ThreadId,
        origin_lock: Option<PiMutexRaw>,
        top_task: ThreadId,
        limit: usize,
    ) -> Result<(), TaskError> {
        let mut current = start;
        for depth in 1..=limit {
            let current_core = self.pi_thread_core(current)?;
            let Some(_activity) = current_core.try_scheduler_activity() else {
                return Err(TaskError::InvalidPiWaitState(
                    PiWaitStateError::ExitedParticipant,
                ));
            };
            if depth == 1
                && let Some(origin) = origin_lock
            {
                let _origin_state = unsafe {
                    // SAFETY: the committed top-task registration keeps the
                    // origin mutex alive for this complete chain walk.
                    origin.lock_state()
                };
                let origin_owner = unsafe {
                    // SAFETY: identical lifetime contract to `_origin_state`.
                    origin.core()
                }
                .owner_snapshot()
                .owner();
                if origin_owner != Some(current) {
                    // Linux aborts rt_mutex_adjust_prio_chain() when the
                    // previous owner released the origin lock after the
                    // caller dropped wait_lock. It may already be queued on
                    // that same lock again, which is a new edge, not a cycle.
                    return Ok(());
                }
                let blocked_on = current_core.sched().lock().pi.blocked_on;
                if blocked_on.is_some_and(|registration| registration.lock == origin) {
                    return Err(TaskError::PiCycle);
                }
            } else {
                let blocked_on = current_core.sched().lock().pi.blocked_on;
                if origin_lock.is_some_and(|origin| {
                    blocked_on.is_some_and(|registration| registration.lock == origin)
                }) {
                    return Err(TaskError::PiCycle);
                }
            }
            let refresh = self.refresh_blocked_waiter_key(&current_core)?;
            if !refresh.changed && origin_lock.is_none() {
                return Ok(());
            }
            let Some(owner) = refresh.owner else {
                return Ok(());
            };
            if owner == top_task {
                return Err(TaskError::PiCycle);
            }
            if depth == limit {
                return Err(TaskError::PiChainLimit { limit });
            }
            current = owner;
        }
        Err(TaskError::PiChainLimit { limit })
    }

    pub(in crate::system::task_system) fn propagate_pi_waiter_key_after_policy_change(
        &self,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        let core = self.pi_thread_core(thread)?;
        let refresh = self.refresh_blocked_waiter_key(&core)?;
        let Some(owner) = refresh.owner else {
            return Ok(());
        };
        if !refresh.changed {
            return Ok(());
        }
        if owner == thread {
            return Err(TaskError::PiCycle);
        }
        self.recompute_pi_cleanup_chain(owner, thread)
    }
}
