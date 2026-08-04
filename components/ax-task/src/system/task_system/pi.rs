//! Priority-inheritance graph transactions.

use core::fmt;

use super::*;
use crate::lock::PreemptTicketGuard;

impl TaskSystemState {
    fn attach_pi_waiter(&mut self, waiter: ThreadId, mut registration: PiWaitRegistration) {
        let owner = registration
            .owner
            .expect("owned PI waiter registration must name its owner");
        let previous_head = self
            .thread_record(owner)
            .expect("prepared PI owner must retain its thread record")
            .pi_waiter_head;
        registration.owner_prev = None;
        registration.owner_next = previous_head;

        if let Some(previous_head) = previous_head {
            let head_registration = self
                .thread_record_mut(previous_head)
                .expect("prepared PI waiter head must retain its thread record")
                .blocked_on
                .as_mut()
                .expect("prepared PI waiter head must retain its registration");
            debug_assert_eq!(head_registration.owner, Some(owner));
            debug_assert_eq!(head_registration.owner_prev, None);
            head_registration.owner_prev = Some(waiter);
        }
        self.thread_record_mut(waiter)
            .expect("prepared PI waiter must retain its thread record")
            .blocked_on = Some(registration);
        self.thread_record_mut(owner)
            .expect("prepared PI owner must retain its thread record")
            .pi_waiter_head = Some(waiter);
    }

    fn detach_pi_waiter(&mut self, waiter: ThreadId) -> PiWaitRegistration {
        let registration = self
            .thread_record(waiter)
            .expect("prepared PI waiter must retain its thread record")
            .blocked_on
            .expect("prepared PI waiter must retain its registration");
        let owner = registration
            .owner
            .expect("owned PI waiter registration must name its owner");

        if let Some(previous) = registration.owner_prev {
            let previous_registration = self
                .thread_record_mut(previous)
                .expect("prepared previous PI waiter must retain its thread record")
                .blocked_on
                .as_mut()
                .expect("prepared previous PI waiter must retain its registration");
            debug_assert_eq!(previous_registration.owner, registration.owner);
            debug_assert_eq!(previous_registration.owner_next, Some(waiter));
            previous_registration.owner_next = registration.owner_next;
        } else {
            let owner = self
                .thread_record_mut(owner)
                .expect("prepared PI owner must retain its thread record");
            debug_assert_eq!(owner.pi_waiter_head, Some(waiter));
            owner.pi_waiter_head = registration.owner_next;
        }

        if let Some(next) = registration.owner_next {
            let next_registration = self
                .thread_record_mut(next)
                .expect("prepared next PI waiter must retain its thread record")
                .blocked_on
                .as_mut()
                .expect("prepared next PI waiter must retain its registration");
            debug_assert_eq!(next_registration.owner, registration.owner);
            debug_assert_eq!(next_registration.owner_prev, Some(waiter));
            next_registration.owner_prev = registration.owner_prev;
        }
        self.thread_record_mut(waiter)
            .expect("prepared PI waiter must retain its thread record")
            .blocked_on = None;
        registration
    }

    fn append_pending_pi_waiter(
        &mut self,
        tail: ThreadId,
        waiter: ThreadId,
        mut registration: PiWaitRegistration,
    ) {
        let tail_registration = self
            .thread_record_mut(tail)
            .expect("pending PI tail must retain its thread record")
            .blocked_on
            .as_mut()
            .expect("pending PI tail must retain its registration");
        debug_assert_eq!(tail_registration.owner, None);
        debug_assert_eq!(tail_registration.owner_next, None);
        tail_registration.owner_next = Some(waiter);

        registration.owner = None;
        registration.owner_prev = Some(tail);
        registration.owner_next = None;
        self.thread_record_mut(waiter)
            .expect("pending PI waiter must retain its thread record")
            .blocked_on = Some(registration);
    }

    fn detach_pending_pi_waiter(&mut self, waiter: ThreadId) -> PiWaitRegistration {
        let registration = self
            .thread_record(waiter)
            .expect("pending PI waiter must retain its thread record")
            .blocked_on
            .expect("pending PI waiter must retain its registration");
        debug_assert_eq!(registration.owner, None);

        if let Some(previous) = registration.owner_prev {
            let previous_registration = self
                .thread_record_mut(previous)
                .expect("pending PI predecessor must retain its thread record")
                .blocked_on
                .as_mut()
                .expect("pending PI predecessor must retain its registration");
            debug_assert_eq!(previous_registration.owner, None);
            debug_assert_eq!(previous_registration.owner_next, Some(waiter));
            previous_registration.owner_next = registration.owner_next;
        }
        if let Some(next) = registration.owner_next {
            let next_registration = self
                .thread_record_mut(next)
                .expect("pending PI successor must retain its thread record")
                .blocked_on
                .as_mut()
                .expect("pending PI successor must retain its registration");
            debug_assert_eq!(next_registration.owner, None);
            debug_assert_eq!(next_registration.owner_prev, Some(waiter));
            next_registration.owner_prev = registration.owner_prev;
        }
        self.thread_record_mut(waiter)
            .expect("pending PI waiter must retain its thread record")
            .blocked_on = None;
        registration
    }
}

/// Prepared scheduler half of one PI mutex ownership transfer.
///
/// Preparation retains the task-system registry lock after validating every
/// fallible scheduler transition. The mutex implementation can then publish
/// its local owner and waiter grant before committing this transaction without
/// exposing a fallible operation between local publication and scheduler
/// publication.
#[must_use = "a prepared PI handoff must be committed after local publication or dropped"]
pub struct PiMutexHandoff<'system> {
    state: PreemptTicketGuard<'system, TaskSystemState>,
    fair_slice_ns: u64,
    lock: PiLockId,
    old_owner: ThreadId,
    next_owner: Option<ThreadId>,
    active_waiters: usize,
    next_waiter_count: Option<usize>,
    old_recompute: PiRecomputeProof,
    next_recompute: Option<PiRecomputeProof>,
}

impl fmt::Debug for PiMutexHandoff<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PiMutexHandoff")
            .field("lock", &self.lock)
            .field("old_owner", &self.old_owner)
            .field("next_owner", &self.next_owner)
            .field("active_waiters", &self.active_waiters)
            .finish_non_exhaustive()
    }
}

impl PiMutexHandoff<'_> {
    /// Commits the prevalidated scheduler transition.
    ///
    /// # Safety
    ///
    /// Before calling this method, the owning PI mutex must publish
    /// `next_owner` as its local owner and publish the matching local waiter
    /// grant. It must keep its metadata lock held until this method returns.
    /// When `next_owner` is `None`, it must publish an unlocked local state.
    pub unsafe fn commit_after_local_handoff(self) {
        let Self {
            mut state,
            fair_slice_ns,
            lock,
            old_owner,
            next_owner,
            active_waiters,
            next_waiter_count,
            old_recompute,
            next_recompute,
        } = self;

        {
            let record = state
                .thread_record(old_owner)
                .expect("prepared PI owner must retain its thread record");
            let mut sched = record.sched.lock();
            debug_assert!(sched.pi.blocked_waiters >= active_waiters);
            sched.pi.blocked_waiters -= active_waiters;
        }

        if let Some(next) = next_owner {
            let mut cursor = state
                .thread_record(old_owner)
                .expect("prepared PI owner must retain its thread record")
                .pi_waiter_head;
            let mut remaining = state.slots.len();
            let mut selected_granted = false;
            while let Some(waiter) = cursor {
                assert!(remaining != 0, "prepared PI waiter list must be acyclic");
                let registration = state
                    .thread_record(waiter)
                    .expect("prepared PI waiter must retain its thread record")
                    .blocked_on
                    .expect("prepared PI waiter must retain its registration");
                cursor = registration.owner_next;
                remaining -= 1;
                if registration.lock != lock {
                    continue;
                }

                let mut registration = state.detach_pi_waiter(waiter);
                if waiter == next {
                    let generation = registration.generation;
                    state
                        .thread_record(waiter)
                        .expect("prepared PI waiter must retain its thread record")
                        .core
                        .pi_wait_state()
                        .grant(generation)
                        .expect("prepared PI waiter generation must remain current");
                    selected_granted = true;
                } else {
                    registration.owner = Some(next);
                    state.attach_pi_waiter(waiter, registration);
                }
            }
            debug_assert!(selected_granted);
            state
                .thread_record(next)
                .expect("prepared PI next owner must retain its thread record")
                .sched
                .lock()
                .pi
                .blocked_waiters =
                next_waiter_count.expect("prepared PI next-owner count must exist");
        }

        state.apply_pi_recompute_chain(old_recompute, fair_slice_ns);
        if let Some(proof) = next_recompute {
            state.apply_pi_recompute_chain(proof, fair_slice_ns);
        }
    }
}

/// Prepared scheduler half of releasing a contended PI mutex.
///
/// The release removes every waiter of this lock from the old owner's
/// donation tree and links them into an ownerless, lock-local pending chain.
/// The selected waiter is only marked for wake; ownership is established by a
/// later [`PiMutexClaim`] transaction.
#[must_use = "a prepared PI release must be committed after local publication or dropped"]
pub struct PiMutexRelease<'system> {
    state: PreemptTicketGuard<'system, TaskSystemState>,
    fair_slice_ns: u64,
    lock: PiLockId,
    old_owner: ThreadId,
    selected: ThreadId,
    active_waiters: usize,
    old_recompute: PiRecomputeProof,
}

impl fmt::Debug for PiMutexRelease<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PiMutexRelease")
            .field("lock", &self.lock)
            .field("old_owner", &self.old_owner)
            .field("selected", &self.selected)
            .field("active_waiters", &self.active_waiters)
            .finish_non_exhaustive()
    }
}

impl PiMutexRelease<'_> {
    /// Removes the old donation owner and publishes wake selection.
    ///
    /// # Safety
    ///
    /// The owning mutex must already have published the ownerless
    /// `HAS_WAITERS` owner word and stored `selected` as its pending-chain
    /// anchor. The local publication must still match the state validated by
    /// [`TaskSystem::prepare_pi_mutex_release`]. No mutex-local lock may be
    /// held while this scheduler transaction is committed.
    pub unsafe fn commit_after_local_release(self) {
        let Self {
            mut state,
            fair_slice_ns,
            lock,
            old_owner,
            selected,
            active_waiters,
            old_recompute,
        } = self;

        {
            let record = state
                .thread_record(old_owner)
                .expect("prepared PI owner must retain its thread record");
            let mut sched = record.sched.lock();
            debug_assert!(sched.pi.blocked_waiters >= active_waiters);
            sched.pi.blocked_waiters -= active_waiters;
        }

        let mut selected_registration = state.detach_pi_waiter(selected);
        debug_assert_eq!(selected_registration.lock, lock);
        selected_registration.owner = None;
        selected_registration.owner_prev = None;
        selected_registration.owner_next = None;
        let selected_generation = selected_registration.generation;
        state
            .thread_record_mut(selected)
            .expect("selected PI waiter must retain its thread record")
            .blocked_on = Some(selected_registration);

        let mut pending_tail = selected;
        let mut moved = 1usize;
        let mut cursor = state
            .thread_record(old_owner)
            .expect("prepared PI owner must retain its thread record")
            .pi_waiter_head;
        let mut remaining = state.slots.len();
        while let Some(waiter) = cursor {
            assert!(remaining != 0, "prepared PI waiter list must be acyclic");
            let registration = state
                .thread_record(waiter)
                .expect("prepared PI waiter must retain its thread record")
                .blocked_on
                .expect("prepared PI waiter must retain its registration");
            cursor = registration.owner_next;
            remaining -= 1;
            if registration.lock != lock {
                continue;
            }

            let registration = state.detach_pi_waiter(waiter);
            state.append_pending_pi_waiter(pending_tail, waiter, registration);
            pending_tail = waiter;
            moved += 1;
        }
        debug_assert_eq!(moved, active_waiters);

        state
            .thread_record(selected)
            .expect("selected PI waiter must retain its thread record")
            .core
            .pi_wait_state()
            .select(selected_generation)
            .expect("prepared PI selection generation must remain current");
        state.apply_pi_recompute_chain(old_recompute, fair_slice_ns);
    }
}

/// Prepared scheduler half of claiming an ownerless PI mutex.
#[must_use = "a prepared PI claim must be committed after local ownership publication or dropped"]
pub struct PiMutexClaim<'system> {
    state: PreemptTicketGuard<'system, TaskSystemState>,
    fair_slice_ns: u64,
    lock: PiLockId,
    pending_head: ThreadId,
    claimant: ThreadId,
    active_waiters: usize,
    next_waiter_count: usize,
    next_recompute: PiRecomputeProof,
}

impl fmt::Debug for PiMutexClaim<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PiMutexClaim")
            .field("lock", &self.lock)
            .field("pending_head", &self.pending_head)
            .field("claimant", &self.claimant)
            .field("active_waiters", &self.active_waiters)
            .finish_non_exhaustive()
    }
}

impl PiMutexClaim<'_> {
    /// Attaches remaining waiters to the new owner and grants the claimant.
    ///
    /// # Safety
    ///
    /// The owning mutex must already have removed `claimant` from the local
    /// waiter queue, published `claimant` in the owner word, and cleared its
    /// pending-chain anchor. The local publication must still match the state
    /// validated by [`TaskSystem::prepare_pi_mutex_claim`]. No mutex-local lock
    /// may be held while this scheduler transaction is committed.
    pub unsafe fn commit_after_local_claim(self) {
        let Self {
            mut state,
            fair_slice_ns,
            lock,
            pending_head,
            claimant,
            active_waiters,
            next_waiter_count,
            next_recompute,
        } = self;

        let mut cursor = Some(pending_head);
        let mut remaining = state.slots.len();
        let mut claimed = false;
        let mut moved = 0usize;
        while let Some(waiter) = cursor {
            assert!(remaining != 0, "pending PI waiter list must be acyclic");
            let registration = state
                .thread_record(waiter)
                .expect("pending PI waiter must retain its thread record")
                .blocked_on
                .expect("pending PI waiter must retain its registration");
            debug_assert_eq!(registration.owner, None);
            debug_assert_eq!(registration.lock, lock);
            cursor = registration.owner_next;
            remaining -= 1;

            let mut registration = state.detach_pending_pi_waiter(waiter);
            let wait_state = state
                .thread_record(waiter)
                .expect("pending PI waiter must retain its thread record")
                .core
                .pi_wait_state();
            wait_state.clear_selection(registration.generation);
            if waiter == claimant {
                wait_state
                    .grant(registration.generation)
                    .expect("prepared PI claimant generation must remain current");
                claimed = true;
            } else {
                registration.owner = Some(claimant);
                registration.owner_prev = None;
                registration.owner_next = None;
                state.attach_pi_waiter(waiter, registration);
                moved += 1;
            }
        }
        debug_assert!(claimed);
        debug_assert_eq!(moved + 1, active_waiters);

        state
            .thread_record(claimant)
            .expect("prepared PI claimant must retain its thread record")
            .sched
            .lock()
            .pi
            .blocked_waiters = next_waiter_count;
        state.apply_pi_recompute_chain(next_recompute, fair_slice_ns);
    }
}

impl TaskSystem {
    /// Creates a donation edge and a wake-before-block handshake token.
    pub fn pi_wait_start(
        &self,
        lock: PiLockId,
        waiter: ThreadId,
        owner: ThreadId,
    ) -> Result<PiWaitToken, TaskError> {
        let mut state = self.state.lock();
        if waiter == owner {
            return Err(TaskError::InvalidPiState);
        }
        if state.thread_record(waiter)?.sched.lock().lifecycle.state() == ThreadState::Exited
            || state.thread_record(owner)?.sched.lock().lifecycle.state() == ThreadState::Exited
        {
            return Err(TaskError::InvalidPiState);
        }
        // Like Linux's PI-futex/proxy registration, the scheduler core reports
        // deadlock detection to its caller before publishing a waiter edge.
        // A normal kernel mutex may still treat this as a fatal programming
        // error, but that policy does not belong in the reusable PI graph.
        state.ensure_pi_acyclic(waiter, owner)?;
        let owner_core = Arc::clone(&state.thread_record(owner)?.core);
        let waiter_core = Arc::clone(&state.thread_record(waiter)?.core);
        if state.thread_record(waiter)?.blocked_on.is_some() {
            return Err(TaskError::InvalidPiState);
        }
        state.validate_pi_donor(waiter)?;
        let waiter_urgency = {
            let sched = state.thread_record(waiter)?.sched.lock();
            sched
                .policy
                .effective_entity
                .scheduling_urgency(sched.policy.effective)
        };
        let (next_waiter_count, owner_urgency, rescue_changes) = {
            let sched = state.thread_record(owner)?.sched.lock();
            let next_waiter_count = sched
                .pi
                .blocked_waiters
                .checked_add(1)
                .ok_or(TaskError::InvalidPiState)?;
            let owner_urgency = sched
                .policy
                .effective_entity
                .scheduling_urgency(sched.policy.effective);
            let should_rescue = sched
                .policy
                .effective_entity
                .deadline()
                .is_some_and(|deadline| deadline.remaining_runtime_ns() == 0);
            (
                next_waiter_count,
                owner_urgency,
                should_rescue != sched.pi.critical_rescue,
            )
        };
        // Linux rtmutex keeps every blocked-on edge, but adjusts the owner's
        // effective priority only when the new top waiter can change it.
        // Equal or less urgent registrations must therefore stay local to the
        // graph update rather than rescanning the complete donation chain.
        let recompute = (waiter_urgency < owner_urgency || rescue_changes)
            .then(|| state.prepare_pi_recompute_chain(owner))
            .transpose()?;
        let generation = waiter_core.pi_wait_state().begin()?;

        state.attach_pi_waiter(
            waiter,
            PiWaitRegistration {
                lock,
                owner: Some(owner),
                generation,
                owner_prev: None,
                owner_next: None,
            },
        );
        state.thread_record(owner)?.sched.lock().pi.blocked_waiters = next_waiter_count;
        if let Some(recompute) = recompute {
            state.apply_pi_recompute_chain(recompute, self.config.fair_slice_ns());
        }

        Ok(PiWaitToken {
            core: waiter_core,
            initial_owner: Some(owner_core),
            generation,
        })
    }

    /// Registers a waiter which arrived during an ownerless claim window.
    ///
    /// Pending waiters do not donate until one claimant publishes ownership.
    /// `pending_head` is the mutex-local selected waiter anchoring the
    /// generation-checked pending chain.
    pub fn pi_wait_start_pending(
        &self,
        lock: PiLockId,
        waiter: ThreadId,
        pending_head: ThreadId,
    ) -> Result<PiWaitToken, TaskError> {
        let mut state = self.state.lock();
        if waiter == pending_head
            || state.thread_record(waiter)?.sched.lock().lifecycle.state() == ThreadState::Exited
        {
            return Err(TaskError::InvalidPiState);
        }
        let waiter_core = Arc::clone(&state.thread_record(waiter)?.core);
        if state.thread_record(waiter)?.blocked_on.is_some() {
            return Err(TaskError::InvalidPiState);
        }
        state.validate_pi_donor(waiter)?;

        let mut tail = pending_head;
        let mut remaining = state.slots.len();
        loop {
            if remaining == 0 {
                return Err(TaskError::PiCycle);
            }
            let registration = state
                .thread_record(tail)?
                .blocked_on
                .ok_or(TaskError::InvalidPiState)?;
            if registration.owner.is_some() || registration.lock != lock {
                return Err(TaskError::InvalidPiState);
            }
            let Some(next) = registration.owner_next else {
                break;
            };
            tail = next;
            remaining -= 1;
        }

        let generation = waiter_core.pi_wait_state().begin()?;
        state.append_pending_pi_waiter(
            tail,
            waiter,
            PiWaitRegistration {
                lock,
                owner: None,
                generation,
                owner_prev: None,
                owner_next: None,
            },
        );
        Ok(PiWaitToken {
            core: waiter_core,
            initial_owner: None,
            generation,
        })
    }

    /// Cancels a waiter token after a wake-before-block handoff race.
    pub fn pi_wait_cancel(&self, token: PiWaitToken) -> Result<(), TaskError> {
        let mut state = self.state.lock();
        let waiter = token.waiter();
        let registration = state
            .thread_record(waiter)?
            .blocked_on
            .filter(|registration| registration.generation == token.generation)
            .ok_or(TaskError::InvalidPiState)?;
        let Some(owner) = registration.owner else {
            if registration.owner_prev.is_none() {
                // The mutex-local pending head must be replaced under the
                // mutex metadata lock; scheduler-only cancellation cannot
                // safely choose and wake that successor.
                return Err(TaskError::InvalidPiState);
            }
            state.detach_pending_pi_waiter(waiter);
            token.core.pi_wait_state().clear_selection(token.generation);
            return Ok(());
        };
        let recompute = state.prepare_pi_recompute_chain(owner)?;
        let next_waiter_count = state
            .thread_record(owner)?
            .sched
            .lock()
            .pi
            .blocked_waiters
            .checked_sub(1)
            .ok_or(TaskError::InvalidPiState)?;

        state.detach_pi_waiter(waiter);
        state.thread_record(owner)?.sched.lock().pi.blocked_waiters = next_waiter_count;
        state.apply_pi_recompute_chain(recompute, self.config.fair_slice_ns());
        Ok(())
    }

    /// Validates and locks the scheduler half of a contended mutex release.
    ///
    /// Callers must invoke this outside their mutex-local metadata gate, then
    /// reacquire that gate and validate their local sequence before publishing
    /// the ownerless handoff. A stale local snapshot must drop the returned
    /// transaction and retry without changing local ownership.
    pub fn prepare_pi_mutex_release(
        &self,
        lock: PiLockId,
        old_owner: ThreadId,
        selected: ThreadId,
    ) -> Result<PiMutexRelease<'_>, TaskError> {
        let state = self.state.lock();
        let old_recompute = state.prepare_pi_recompute_chain(old_owner)?;
        let mut active_waiters = 0usize;
        let mut selected_registration = None;
        let mut cursor = state.pi_waiter_cursor(old_owner)?;
        while let Some((waiter, registration)) = state.next_pi_waiter(&mut cursor)? {
            if registration.lock != lock {
                continue;
            }
            active_waiters += 1;
            if waiter == selected {
                selected_registration = Some(registration);
            }
        }
        let selected_registration = selected_registration.ok_or(TaskError::InvalidPiState)?;
        if active_waiters == 0
            || !state
                .thread_record(selected)?
                .core
                .pi_wait_state()
                .can_select(selected_registration.generation)
            || state
                .thread_record(old_owner)?
                .sched
                .lock()
                .pi
                .blocked_waiters
                < active_waiters
        {
            return Err(TaskError::InvalidPiState);
        }

        Ok(PiMutexRelease {
            state,
            fair_slice_ns: self.config.fair_slice_ns(),
            lock,
            old_owner,
            selected,
            active_waiters,
            old_recompute,
        })
    }

    /// Validates and locks the scheduler half of an ownerless mutex claim.
    ///
    /// Callers must invoke this outside their mutex-local metadata gate, then
    /// reacquire that gate and validate their local sequence before publishing
    /// the new owner. A stale local snapshot must drop the returned transaction
    /// and retry without changing local ownership.
    pub fn prepare_pi_mutex_claim(
        &self,
        lock: PiLockId,
        pending_head: ThreadId,
        claimant: ThreadId,
    ) -> Result<PiMutexClaim<'_>, TaskError> {
        let state = self.state.lock();
        let next_recompute = state.prepare_pi_recompute_chain(claimant)?;
        let mut cursor = Some(pending_head);
        let mut previous = None;
        let mut active_waiters = 0usize;
        let mut claimant_found = false;
        let mut remaining = state.slots.len();
        while let Some(waiter) = cursor {
            if remaining == 0 {
                return Err(TaskError::PiCycle);
            }
            let record = state.thread_record(waiter)?;
            let registration = record.blocked_on.ok_or(TaskError::InvalidPiState)?;
            if registration.owner.is_some()
                || registration.lock != lock
                || registration.owner_prev != previous
                || !record
                    .core
                    .pi_wait_state()
                    .can_grant(registration.generation)
            {
                return Err(TaskError::InvalidPiState);
            }
            claimant_found |= waiter == claimant;
            active_waiters += 1;
            previous = Some(waiter);
            cursor = registration.owner_next;
            remaining -= 1;
        }
        if !claimant_found {
            return Err(TaskError::InvalidPiState);
        }
        let next_waiter_count = state
            .thread_record(claimant)?
            .sched
            .lock()
            .pi
            .blocked_waiters
            .checked_add(active_waiters.saturating_sub(1))
            .ok_or(TaskError::InvalidPiState)?;

        Ok(PiMutexClaim {
            state,
            fair_slice_ns: self.config.fair_slice_ns(),
            lock,
            pending_head,
            claimant,
            active_waiters,
            next_waiter_count,
            next_recompute,
        })
    }

    /// Validates and locks the scheduler half of one PI mutex handoff.
    ///
    /// The returned transaction owns the task-system registry lock. The caller
    /// must continue to hold the mutex metadata lock acquired before this call,
    /// publish its local transition, and then commit the transaction.
    ///
    /// # Errors
    ///
    /// Returns [`TaskError::InvalidPiState`] for a stale owner, selected waiter,
    /// grant generation, or waiter count. Other registry validation errors are
    /// returned before either scheduler or local state is changed.
    pub fn prepare_pi_mutex_handoff(
        &self,
        lock: PiLockId,
        old_owner: ThreadId,
        next_owner: Option<ThreadId>,
    ) -> Result<PiMutexHandoff<'_>, TaskError> {
        let state = self.state.lock();
        let old_recompute = state.prepare_pi_recompute_chain(old_owner)?;
        let mut active_waiters = 0usize;
        let mut selected_waiter = false;
        let mut cursor = state.pi_waiter_cursor(old_owner)?;
        while let Some((waiter, registration)) = state.next_pi_waiter(&mut cursor)? {
            if registration.lock != lock {
                continue;
            }
            active_waiters += 1;
            selected_waiter |= next_owner == Some(waiter);
        }
        if (active_waiters == 0 && next_owner.is_some())
            || (active_waiters != 0 && !selected_waiter)
        {
            return Err(TaskError::InvalidPiState);
        }

        let next_recompute = next_owner
            .map(|next| state.prepare_pi_recompute_chain(next))
            .transpose()?;
        if let Some(next) = next_owner {
            let record = state.thread_record(next)?;
            let registration = record.blocked_on.ok_or(TaskError::InvalidPiState)?;
            if registration.lock != lock
                || registration.owner != Some(old_owner)
                || !record
                    .core
                    .pi_wait_state()
                    .can_grant(registration.generation)
            {
                return Err(TaskError::InvalidPiState);
            }
        }

        let redirected_waiters = active_waiters.saturating_sub(usize::from(selected_waiter));
        let next_waiter_count = next_owner
            .map(|next| {
                state
                    .thread_record(next)?
                    .sched
                    .lock()
                    .pi
                    .blocked_waiters
                    .checked_add(redirected_waiters)
                    .ok_or(TaskError::InvalidPiState)
            })
            .transpose()?;
        if state
            .thread_record(old_owner)?
            .sched
            .lock()
            .pi
            .blocked_waiters
            < active_waiters
        {
            return Err(TaskError::InvalidPiState);
        }

        Ok(PiMutexHandoff {
            state,
            fair_slice_ns: self.config.fair_slice_ns(),
            lock,
            old_owner,
            next_owner,
            active_waiters,
            next_waiter_count,
            old_recompute,
            next_recompute,
        })
    }
}
