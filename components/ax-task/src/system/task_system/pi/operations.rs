//! Public PI mutex registration, cancellation, release, and claim transactions.

use super::*;

enum PiWaiterRemoval {
    Removed(Option<ThreadId>),
    HandoffPending,
}

impl TaskSystem {
    fn remove_registered_waiter(
        &self,
        waiter_core: &Arc<ThreadCore>,
        lock: PiMutexRaw,
        generation: u64,
        reject_handoff_top: bool,
    ) -> Result<PiWaiterRemoval, TaskError> {
        let mut lock_state = unsafe {
            // SAFETY: the token or rollback caller retains the mutex identity.
            lock_raw_pi_mutex_waiters(lock)
        };
        let core = unsafe {
            // SAFETY: identical lifetime contract to the waiter-tree guard.
            lock.core()
        };
        let snapshot = core.owner_snapshot();
        if !snapshot.has_waiters() {
            return Err(TaskError::InvalidPiState);
        }
        let registration = waiter_core
            .sched()
            .lock()
            .pi
            .blocked_on
            .filter(|registration| {
                registration.lock == lock && registration.generation == generation
            })
            .ok_or(TaskError::InvalidPiState)?;
        if reject_handoff_top
            && snapshot.is_ownerless()
            && lock_state.waiters.first() == Some(registration.key)
        {
            return Ok(PiWaiterRemoval::HandoffPending);
        }
        if !lock_state.waiters.contains(registration.key) {
            return Err(TaskError::InvalidPiState);
        }
        let owner = snapshot.owner().map(ThreadId::from);
        let owner_core = owner.map(|owner| self.pi_thread_core(owner)).transpose()?;
        self.remove_lock_waiter(
            &mut lock_state,
            owner_core.as_ref(),
            waiter_core,
            generation,
        )?;
        if lock_state.waiters.is_empty() {
            if let Some(owner) = owner {
                core.clear_waiters_bit(owner.into());
            } else {
                core.publish_unlocked();
            }
        }
        Ok(PiWaiterRemoval::Removed(owner))
    }

    /// Registers one contender in the mutex-owned PI waiter tree.
    pub fn pi_mutex_lock_slow(
        &self,
        lock: PiMutexRef<'_>,
        waiter: ThreadId,
        sequence: u64,
    ) -> Result<PiMutexLockResult, TaskError> {
        let _preempt = PreemptScope::enter();
        let waiter_core = self.pi_thread_core(waiter)?;
        let Some(_waiter_activity) = waiter_core.try_scheduler_activity() else {
            return Err(TaskError::InvalidPiWaitState(
                PiWaitStateError::ExitedParticipant,
            ));
        };
        let (donation_policy, donation_root) = {
            let sched = waiter_core.sched().lock();
            if sched.lifecycle.state() == ThreadState::Exited {
                return Err(TaskError::InvalidPiWaitState(
                    PiWaitStateError::ExitedParticipant,
                ));
            }
            if sched.pi.blocked_on.is_some() {
                return Err(TaskError::InvalidPiWaitState(
                    PiWaitStateError::WaiterAlreadyBlocked,
                ));
            }
            (
                waiter_core.effective_policy_snapshot(),
                sched.pi.donor.unwrap_or(waiter_core.id()),
            )
        };
        let lock_raw = lock.raw();
        let mutex_core = lock.core();
        let urgency = waiter_core.effective_pi_wait_urgency();
        let donation =
            self.pi_donation_from_snapshot(&waiter_core, donation_policy, donation_root)?;
        let key = PiWaitKey::new(urgency, sequence, waiter);
        let mut lock_state = lock_pi_mutex_waiters(lock);
        loop {
            let snapshot = mutex_core.owner_snapshot();
            if snapshot.is_unlocked() {
                if !lock_state.waiters.is_empty() {
                    return Err(TaskError::InvalidPiWaitState(
                        PiWaitStateError::StaleSchedulerOwnership,
                    ));
                }
                if mutex_core.try_acquire_snapshot(snapshot, waiter.into()) {
                    return Ok(PiMutexLockResult::Acquired);
                }
                continue;
            }
            let owner = snapshot.owner().map(ThreadId::from);
            if owner == Some(waiter) {
                return Err(TaskError::InvalidPiWaitState(
                    PiWaitStateError::WaiterOwnsLock,
                ));
            }
            if snapshot.has_waiters() != !lock_state.waiters.is_empty()
                || snapshot.is_ownerless() && lock_state.waiters.is_empty()
            {
                return Err(TaskError::InvalidPiWaitState(
                    PiWaitStateError::StaleSchedulerOwnership,
                ));
            }
            if owner.is_none() && !snapshot.is_ownerless() {
                return Err(TaskError::InvalidPiWaitState(
                    PiWaitStateError::OwnerlessSelectionMissing,
                ));
            }
            // Linux rereads the futex owner after an exit-race lookup fails:
            // an owner may release this mutex and close its scheduler lifetime
            // after our physical snapshot but before we lease its PI state.
            // Only an unchanged snapshot still describes an exited owner.
            let initial_owner = if let Some(owner) = owner {
                match self.pi_thread_core(owner) {
                    Ok(owner) => Some(owner),
                    Err(_) if mutex_core.owner_snapshot() != snapshot => continue,
                    Err(_) => {
                        return Err(TaskError::InvalidPiWaitState(
                            PiWaitStateError::ExitedParticipant,
                        ));
                    }
                }
            } else {
                None
            };

            let _owner_activity = if let Some(owner) = initial_owner.as_ref() {
                match owner.try_scheduler_activity() {
                    Some(activity) => Some(activity),
                    None if mutex_core.owner_snapshot() != snapshot => continue,
                    None => {
                        return Err(TaskError::InvalidPiWaitState(
                            PiWaitStateError::ExitedParticipant,
                        ));
                    }
                }
            } else {
                None
            };
            if !mutex_core.try_mark_waiters(snapshot) {
                continue;
            }
            let generation = match waiter_core.pi_wait_state().begin() {
                Ok(generation) => generation,
                Err(error) => {
                    if !snapshot.has_waiters() {
                        mutex_core.clear_waiters_bit(
                            owner.expect("an owned mutex must retain its owner").into(),
                        );
                    }
                    return Err(error);
                }
            };
            let owner_next_lock = match self.insert_lock_waiter(
                &mut lock_state,
                initial_owner.as_ref(),
                &waiter_core,
                PiWaitRegistration {
                    lock: lock_raw,
                    key,
                    generation,
                },
                donation.with_wait_generation(generation),
            ) {
                Ok(owner_next_lock) => owner_next_lock,
                Err(error) => {
                    if lock_state.waiters.is_empty() {
                        mutex_core.clear_waiters_bit(
                            owner
                                .expect("an empty waiter tree must retain owner")
                                .into(),
                        );
                    }
                    return Err(error);
                }
            };
            // Linux snapshots `owner->pi_blocked_on->lock` while both the
            // origin wait-lock and owner pi-lock protect the newly installed
            // edge. A later `blocked_on` value can belong to an unrelated
            // dependency created after the owner releases this mutex.
            drop(lock_state);
            let chain_result =
                if let (Some(owner), Some(owner_next_lock)) = (owner, owner_next_lock) {
                    self.recompute_pi_chain(owner, lock_raw, owner_next_lock, waiter)
                } else {
                    Ok(())
                };
            if let Err(error) = chain_result {
                let rollback_owner = self
                    .remove_registered_waiter(&waiter_core, lock_raw, generation, false)
                    .unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(
                            0x5049_120e,
                            waiter_core.id().as_u64() as usize,
                        )
                    });
                let PiWaiterRemoval::Removed(rollback_owner) = rollback_owner else {
                    task_runtime::fatal_invariant(0x5049_1216, waiter_core.id().as_u64() as usize)
                };
                if let Some(rollback_owner) = rollback_owner {
                    self.recompute_pi_cleanup_chain(rollback_owner, waiter)
                        .unwrap_or_else(|_| {
                            task_runtime::fatal_invariant(
                                0x5049_120f,
                                waiter_core.id().as_u64() as usize,
                            )
                        });
                }
                return Err(error);
            }
            // Linux takes a task_struct reference while the owner remains
            // protected by wait_lock/pi_lock. Retain the equivalent typed
            // scheduler handle before releasing the owner activity lease, so
            // owner observation never has to resolve a stale numeric ID.
            let initial_owner_handle = initial_owner
                .as_ref()
                .map(|owner| ThreadHandle::from_core(Arc::clone(owner)));
            #[cfg(axtest)]
            super::axtest::record_waiter_registration(owner, waiter_core.state());
            drop(_owner_activity);
            drop(_waiter_activity);

            return Ok(PiMutexLockResult::Waiting(unsafe {
                // SAFETY: both waiter-tree edges are committed and retain this
                // physical lock identity until claim or cancellation.
                PiWaitToken::from_registration(
                    lock_raw,
                    waiter.into(),
                    initial_owner_handle,
                    generation,
                    core::ptr::NonNull::from(waiter_core.pi_wait_state()).cast(),
                )
            }));
        }
    }

    /// Cancels a committed waiter which has not been selected for claim.
    pub fn pi_wait_cancel(&self, token: PiWaitToken) -> Result<(), TaskError> {
        match self.pi_wait_try_cancel(&token)? {
            PiWaitCancelOutcome::Cancelled => Ok(()),
            PiWaitCancelOutcome::HandoffPending => Err(TaskError::InvalidPiState),
        }
    }

    /// Tries to cancel a committed waiter without consuming a published
    /// ownerless handoff.
    pub fn pi_wait_try_cancel(
        &self,
        token: &PiWaitToken,
    ) -> Result<PiWaitCancelOutcome, TaskError> {
        let _preempt = PreemptScope::enter();
        let waiter = ThreadId::from(token.thread_id());
        let waiter_core = self.pi_thread_core(waiter)?;
        let removal = self.remove_registered_waiter(
            &waiter_core,
            token.lock_raw(),
            token.generation(),
            true,
        )?;
        let PiWaiterRemoval::Removed(owner) = removal else {
            return Ok(PiWaitCancelOutcome::HandoffPending);
        };
        if let Some(owner) = owner {
            self.recompute_pi_cleanup_chain(owner, waiter)?;
        }
        Ok(PiWaitCancelOutcome::Cancelled)
    }

    /// Publishes an ownerless handoff and wakes the current top waiter.
    pub fn pi_mutex_release(
        &self,
        lock: PiMutexRef<'_>,
        old_owner: ThreadId,
    ) -> Result<(), TaskError> {
        let _preempt = PreemptScope::enter();
        let old_owner_core = self.pi_thread_core(old_owner)?;
        let mut wakes = ThreadWakeBatch::new();
        let selected_id = loop {
            let mutex_core = lock.core();
            let lock_state = lock_pi_mutex_waiters(lock);
            let snapshot = mutex_core.owner_snapshot();
            if snapshot.owner() != Some(old_owner.into()) {
                return Err(TaskError::InvalidPiState);
            }
            if !snapshot.has_waiters() {
                if !lock_state.waiters.is_empty() {
                    return Err(TaskError::InvalidPiState);
                }
                // Match Linux rt_mutex_slowunlock(): once the final waiter
                // cancels, drop wait_lock before the owner-word CAS. A waiter
                // which marks the owner in that window makes the CAS fail, so
                // slow unlock retakes wait_lock and selects the new top waiter.
                drop(lock_state);
                let released = unsafe {
                    // SAFETY: `old_owner` came from the owner-authorized raw
                    // release transition, and this loop retains that authority.
                    mutex_core.try_release_for_thread(old_owner)
                }?;
                if released {
                    return Ok(());
                }
                continue;
            }
            let selected_entry = lock_state
                .waiters
                .first_entry()
                .ok_or(TaskError::InvalidPiState)?;
            let selected_key = selected_entry.0;
            let selected = selected_entry
                .1
                .waiter_core()
                .ok_or(TaskError::InvalidPiState)?;
            let selected_generation = selected_entry
                .1
                .wait_generation()
                .ok_or(TaskError::InvalidPiState)?;
            if selected.id() != selected_key.thread
                || !selected.pi_wait_state().can_grant(selected_generation)
            {
                return Err(TaskError::InvalidPiState);
            }
            {
                let mut old_owner_sched = old_owner_core.sched().lock();
                if !old_owner_sched.pi.donors.contains(selected_key) {
                    return Err(TaskError::InvalidPiState);
                }
                self.replace_owner_lock_top_locked(
                    &old_owner_core,
                    &mut old_owner_sched,
                    Some(selected_entry),
                    None,
                )?;
            }
            mutex_core.publish_ownerless();
            let selected_id = selected.id();
            let _queued = wakes.push(ThreadWakeHandle::from_core(selected));
            drop(lock_state);
            break selected_id;
        };
        self.recompute_pi_cleanup_chain(old_owner, selected_id)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5049_1211, old_owner.as_u64() as usize)
            });

        // The ownerless publication and waiter generation commit the mutex
        // handoff. As with Linux wake_q, this delayed wake is only a scheduling
        // hint: the selected waiter may claim, run, and exit before the batch
        // drains. The batch retains its task reference and intentionally
        // ignores a wake that no longer changes task state.
        let _woken = wakes.wake_all();
        Ok(())
    }

    /// Claims an ownerless handoff selected for this waiter.
    pub fn pi_mutex_claim(&self, token: &PiWaitToken) -> Result<PiMutexClaimOutcome, TaskError> {
        let _preempt = PreemptScope::enter();
        let claimant = ThreadId::from(token.thread_id());
        let claimant_core = self.pi_thread_core(claimant)?;
        let lock = token.lock_raw();
        let mut lock_state = unsafe {
            // SAFETY: the borrowed token keeps the physical mutex core live.
            lock_raw_pi_mutex_waiters(lock)
        };
        let mutex_core = unsafe {
            // SAFETY: the token lifetime is borrowed from this mutex core.
            lock.core()
        };
        if !mutex_core.owner_snapshot().is_ownerless() {
            return Ok(PiMutexClaimOutcome::Retry);
        }
        let Some(registration) = self.claim_ownerless_lock_waiter(
            &mut lock_state,
            &claimant_core,
            lock,
            token.generation(),
        )?
        else {
            return Ok(PiMutexClaimOutcome::Retry);
        };
        debug_assert_eq!(registration.generation, token.generation());
        mutex_core.publish_owner(claimant.into(), !lock_state.waiters.is_empty());
        claimant_core
            .pi_wait_state()
            .grant(token.generation())
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5049_1213, claimant.as_u64() as usize)
            });
        drop(lock_state);
        self.recompute_pi_cleanup_chain(claimant, claimant)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5049_1214, claimant.as_u64() as usize)
            });
        Ok(PiMutexClaimOutcome::Claimed)
    }

    pub(crate) fn pi_initial_owner_is_on_cpu(
        &self,
        token: &PiWaitToken,
    ) -> Result<bool, TaskError> {
        let Some(owner) = token.initial_owner() else {
            return Ok(false);
        };
        let owner = token
            .initial_owner_handle()
            .filter(|handle| handle.id() == ThreadId::from(owner))
            .ok_or(TaskError::InvalidPiState)?
            .runtime_core_arc();
        Ok(owner.sched().scheduler_fence_cpu().is_some())
    }
}
