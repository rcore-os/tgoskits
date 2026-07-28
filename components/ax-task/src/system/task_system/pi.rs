//! Priority-inheritance graph transactions.

use core::fmt;

use super::*;
use crate::lock::IrqTicketGuard;

impl TaskSystemState {
    fn attach_pi_waiter(&mut self, waiter: ThreadId, mut registration: PiWaitRegistration) {
        let owner = registration.owner;
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
            debug_assert_eq!(head_registration.owner, owner);
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
                .thread_record_mut(registration.owner)
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
    state: IrqTicketGuard<'system, TaskSystemState>,
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
            debug_assert!(sched.blocked_pi_waiters >= active_waiters);
            sched.blocked_pi_waiters -= active_waiters;
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
                    registration.owner = next;
                    state.attach_pi_waiter(waiter, registration);
                }
            }
            debug_assert!(selected_granted);
            state
                .thread_record(next)
                .expect("prepared PI next owner must retain its thread record")
                .sched
                .lock()
                .blocked_pi_waiters =
                next_waiter_count.expect("prepared PI next-owner count must exist");
        }

        state.apply_pi_recompute_chain(old_recompute, fair_slice_ns);
        if let Some(proof) = next_recompute {
            state.apply_pi_recompute_chain(proof, fair_slice_ns);
        }
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
        match state.ensure_pi_acyclic(waiter, owner) {
            Ok(()) => {}
            Err(TaskError::PiCycle) => {
                drop(state);
                task_runtime::fatal_invariant(0x5049_0001, waiter.as_u64() as usize);
            }
            Err(error) => return Err(error),
        }
        state.thread_record(owner)?;
        let waiter_core = Arc::clone(&state.thread_record(waiter)?.core);
        if state.thread_record(waiter)?.blocked_on.is_some() {
            return Err(TaskError::InvalidPiState);
        }
        state.validate_pi_donor(waiter)?;
        let recompute = state.prepare_pi_recompute_chain(owner)?;
        let next_waiter_count = state
            .thread_record(owner)?
            .sched
            .lock()
            .blocked_pi_waiters
            .checked_add(1)
            .ok_or(TaskError::InvalidPiState)?;
        let generation = waiter_core.pi_wait_state().begin()?;

        state.attach_pi_waiter(
            waiter,
            PiWaitRegistration {
                lock,
                owner,
                generation,
                owner_prev: None,
                owner_next: None,
            },
        );
        state.thread_record(owner)?.sched.lock().blocked_pi_waiters = next_waiter_count;
        state.apply_pi_recompute_chain(recompute, self.config.fair_slice_ns());

        Ok(PiWaitToken {
            core: waiter_core,
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
        let recompute = state.prepare_pi_recompute_chain(registration.owner)?;
        let next_waiter_count = state
            .thread_record(registration.owner)?
            .sched
            .lock()
            .blocked_pi_waiters
            .checked_sub(1)
            .ok_or(TaskError::InvalidPiState)?;

        state.detach_pi_waiter(waiter);
        state
            .thread_record(registration.owner)?
            .sched
            .lock()
            .blocked_pi_waiters = next_waiter_count;
        state.apply_pi_recompute_chain(recompute, self.config.fair_slice_ns());
        Ok(())
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
                || registration.owner != old_owner
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
                    .blocked_pi_waiters
                    .checked_add(redirected_waiters)
                    .ok_or(TaskError::InvalidPiState)
            })
            .transpose()?;
        if state
            .thread_record(old_owner)?
            .sched
            .lock()
            .blocked_pi_waiters
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
