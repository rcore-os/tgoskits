//! CPU-local scheduler allocation and online publication.

use super::*;

impl TaskSystem {
    /// Allocates one pinned CPU-local scheduler object without publishing it.
    pub fn create_cpu_local(
        &self,
        cpu: CpuId,
    ) -> Result<Pin<alloc::boxed::Box<CpuLocal>>, TaskError> {
        let remote = Arc::clone(&self.state.lock().cpu_registration(cpu)?.remote);
        Ok(CpuLocal::create(
            cpu,
            self.config,
            remote,
            Arc::clone(self.root_domain.rt_bandwidth()),
        ))
    }

    /// Returns the stable remote-publication endpoint of a placement-active CPU.
    pub fn cpu_remote(&self, cpu: CpuId) -> Option<&CpuRemote> {
        self.cpu_remotes
            .get(cpu.as_usize())
            .map(Arc::as_ref)
            .filter(|remote| remote.accepts_placement())
    }

    /// Returns the opaque runtime endpoint for a configured CPU.
    ///
    /// This bootstrap capability is available before online publication so a
    /// runtime can cache its current-CPU endpoint in architecture-owned
    /// storage. Ordinary scheduler producers must use [`Self::cpu_remote`],
    /// which rejects an offline CPU.
    #[doc(hidden)]
    pub fn runtime_cpu_remote_handle(&self, cpu: CpuId) -> CpuRemoteHandle {
        self.cpu_remotes
            .get(cpu.as_usize())
            .map_or(CpuRemoteHandle::NONE, |remote| {
                // SAFETY: TaskSystem retains this Arc allocation until the
                // system is destroyed. Runtime providers may publish the raw
                // handle only while they retain that TaskSystem lifetime.
                unsafe { CpuRemoteHandle::from_raw(Arc::as_ptr(remote).expose_provenance()) }
            })
    }

    /// Returns cumulative non-idle runtime charged by one online CPU.
    pub fn cpu_busy_runtime_ns(&self, cpu: CpuId) -> Result<u64, TaskError> {
        let remote = self
            .cpu_remotes
            .get(cpu.as_usize())
            .ok_or(TaskError::InvalidCpu(cpu.as_u32()))?;
        if !remote.is_online() {
            return Err(TaskError::CpuOffline(cpu.as_u32()));
        }
        Ok(remote.busy_runtime_ns())
    }

    pub(super) fn ensure_owner_cpu_online(&self, cpu: &CpuLocal) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(cpu)?;
        let remote = self
            .cpu_remotes
            .get(cpu.owner().as_usize())
            .ok_or(TaskError::InvalidCpu(cpu.owner().as_u32()))?;
        if Arc::ptr_eq(remote, cpu.remote()) && remote.is_online() {
            Ok(())
        } else {
            Err(TaskError::CpuOffline(cpu.owner().as_u32()))
        }
    }

    /// Enforces the post-publication owner-CPU access contract.
    ///
    /// Standalone scheduler models deliberately operate on an unpublished
    /// `TaskSystem` and retain their direct pinned CpuLocal allocation. Once a
    /// runtime publishes this exact system handle, every online owner access
    /// must instead retain either its IRQ pin or scheduler baton. This mirrors
    /// Linux's rq-lock assertion and closes interrupt-return re-entry over a
    /// live mutable runqueue borrow.
    pub(super) fn ensure_owner_cpu_context(&self, cpu: &CpuLocal) -> Result<(), TaskError> {
        if !cpu.is_online() {
            return Ok(());
        }
        // SAFETY: reading the opaque handle neither dereferences it nor extends
        // its lifetime. Equality only determines whether this model instance
        // has crossed the runtime publication boundary.
        let published = unsafe { task_runtime::task_system_handle() }.into_raw();
        let this = (self as *const Self).expose_provenance();
        if published == 0 || published != this {
            return Ok(());
        }
        match task_runtime::validate_owner_cpu_context() {
            RuntimeStatus::Success => Ok(()),
            RuntimeStatus::UnsafeContext => Err(TaskError::UnsafeContext),
            status => Err(TaskError::RuntimeFailure(status as u32)),
        }
    }

    /// Completes CPU registration and publishes it in the online root domain.
    pub fn bring_cpu_online(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        let _irq = IrqScope::enter();
        self.ensure_owner_cpu_context(&cpu)?;
        let id = cpu.owner();
        let state = self.state.lock();
        let mut root_domain = self.root_domain.lock();
        let registration = state.cpu_registration(id)?;
        if registration.remote.lifecycle_state() != crate::CpuLifecycleState::Offline {
            return Err(TaskError::CpuAlreadyOnline(id.as_u32()));
        }
        if !Arc::ptr_eq(&registration.remote, cpu.remote()) {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        if root_domain.online.contains(id) {
            return Err(TaskError::InvalidConfiguration);
        }
        if state
            .slots
            .iter()
            .filter_map(|slot| slot.record.as_ref())
            .any(|record| {
                let sched = record.sched.lock();
                (matches!(sched.policy.base, SchedulePolicy::Deadline(_))
                    || matches!(sched.policy.requested_policy(), SchedulePolicy::Deadline(_)))
                    && !sched.affinity.affinity.contains(id)
            })
        {
            return Err(TaskError::DeadlineAffinity);
        }
        ensure_runtime_success(task_runtime::prepare_cpu_online(RuntimeCpuId::new(
            id.as_u32(),
        )))?;
        let monotonic_now = task_runtime::monotonic_now();
        cpu.as_mut()
            .reset_fair_balance(monotonic_now, self.config.balance_interval_ns());
        let online_count = root_domain
            .online
            .count()
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        let deadline_rebuild = state.deadline_bandwidth_rebuild(online_count)?;
        self.root_domain.enable_rt_runtime(id);
        assert!(
            root_domain.insert_online(id, deadline_rebuild),
            "validated offline CPU must be absent from the root domain"
        );
        assert!(
            cpu.as_ref().get_ref().remote().mark_online(),
            "validated offline CPU must accept final publication"
        );
        OwnerRqTxn::begin(self, cpu.remote()).commit();
        if cpu
            .lock_run_queue(RunQueueGuardSource::Lifecycle)
            .has_runnable_rt()
        {
            self.root_domain.activate_rt_period(id, monotonic_now);
        }
        Ok(())
    }

    /// Removes a quiescent owner CPU from placement and remote publication.
    ///
    /// The caller must first migrate or retire every non-idle thread, cancel
    /// local task deadlines, and consume the CPU's scheduler IPI. The packed
    /// remote lifecycle closes publication only when its active publisher count
    /// is zero, so a successful transition cannot strand an inbox node between
    /// queue insertion and its doorbell.
    pub fn take_cpu_offline(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let _irq = IrqScope::enter();
        let id = cpu.owner();
        let state = self.state.lock();
        let mut root_domain = self.root_domain.lock();
        let remote = Arc::clone(&state.cpu_registration(id)?.remote);
        if !Arc::ptr_eq(&remote, cpu.remote()) {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        match remote.lifecycle_state() {
            crate::CpuLifecycleState::Offline => return Err(TaskError::CpuOffline(id.as_u32())),
            crate::CpuLifecycleState::Inactive | crate::CpuLifecycleState::Draining => {
                return Err(TaskError::CpuNotQuiescent(id.as_u32()));
            }
            crate::CpuLifecycleState::Online => {}
        }
        if root_domain.online.count() <= 1 {
            return Err(TaskError::LastOnlineCpu(id.as_u32()));
        }
        if !root_domain.can_deactivate_cpu(id) {
            return Err(TaskError::DeadlineAdmission);
        }
        let remaining_online = root_domain
            .online
            .count()
            .checked_sub(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        let deadline_rebuild = state.deadline_bandwidth_rebuild(remaining_online)?;
        let rt_period_replacement = (0..root_domain.online.topology_len())
            .map(|index| CpuId::new(index as u32))
            .find(|candidate| *candidate != id && root_domain.online.contains(*candidate))
            .ok_or(TaskError::LastOnlineCpu(id.as_u32()))?;

        self.migrate_dormant_deadline_bandwidth_for_cpu_offline(&state, &root_domain, id)?;

        if !remote.try_deactivate() {
            Err(TaskError::CpuNotQuiescent(id.as_u32()))
        } else if !Self::prepare_thread_targets_for_cpu_offline(&state, &root_domain, id)
            || !remote.try_begin_draining()
        {
            remote.cancel_deactivation();
            Err(TaskError::CpuNotQuiescent(id.as_u32()))
        } else if !cpu.is_quiescent_for_offline()
            || !Self::threads_allow_cpu_offline(&state, &root_domain, id)
        {
            remote.cancel_draining();
            Err(TaskError::CpuNotQuiescent(id.as_u32()))
        } else if let Err(error) = ensure_runtime_success(task_runtime::prepare_cpu_offline(
            RuntimeCpuId::new(id.as_u32()),
        )) {
            remote.cancel_draining();
            Err(error)
        } else if !root_domain.remove_online(id, deadline_rebuild) {
            remote.cancel_draining();
            Err(TaskError::InvalidConfiguration)
        } else {
            cpu.as_mut().clear_fair_balance();
            self.root_domain.disable_rt_runtime(id);
            remote.finish_offline();
            self.root_domain.publish_offline(id);
            if self
                .root_domain
                .rt_bandwidth()
                .migrate_owner(id, rt_period_replacement)
            {
                self.cpu_remotes[rt_period_replacement.as_usize()].kick_scheduler_work();
            }
            Ok(())
        }
    }

    /// Mirrors Linux `dl_task_offline_migration()` for blocked DL tasks.
    ///
    /// Runnable/on-CPU tasks must already leave through the normal placement
    /// carrier. A dormant reservation, however, still owns `this_bw`, optional
    /// `running_bw`, and inactive/CBS timer entries. Those facts move together
    /// before the source CPU closes placement publication.
    fn migrate_dormant_deadline_bandwidth_for_cpu_offline(
        &self,
        state: &TaskSystemState,
        root_domain: &RootDomainState,
        source: CpuId,
    ) -> Result<(), TaskError> {
        let source_remote = &self.cpu_remotes[source.as_usize()];
        for record in state.slots.iter().filter_map(|slot| slot.record.as_ref()) {
            let core = &record.core;
            let mut sched = record.sched.lock();
            if sched.deadline.bandwidth.reservation_owner() != Some(source) {
                continue;
            }
            if sched.placement.queued_cpu().is_some()
                || sched.placement.execution_cpu().is_some()
                || sched.placement.on_cpu().is_some()
                || sched.placement.has_pending_migration()
            {
                return Err(TaskError::CpuNotQuiescent(source.as_u32()));
            }
            let target = (0..root_domain.online.topology_len())
                .map(|index| CpuId::new(index as u32))
                .find(|candidate| {
                    *candidate != source
                        && root_domain.online.contains(*candidate)
                        && sched.affinity.affinity.contains(*candidate)
                        && self.cpu_remotes[candidate.as_usize()].accepts_placement()
                })
                .ok_or(TaskError::DeadlineAffinity)?;
            let target_remote = &self.cpu_remotes[target.as_usize()];
            let publication = target_remote
                .begin_publication()
                .ok_or(TaskError::CpuNotQuiescent(target.as_u32()))?;

            let mut source_rq = OwnerRqTxn::begin(self, source_remote);
            Self::detach_owner_deadline_bandwidth_in_rq(
                core,
                &mut sched,
                source_remote,
                &mut source_rq,
            );
            source_rq.commit();

            let active = sched.deadline.bandwidth.is_active();
            let mut target_rq = OwnerRqTxn::begin(self, target_remote);
            Self::attach_deadline_bandwidth_locked(
                core,
                &mut sched,
                &mut target_rq,
                target,
                active,
            );
            target_rq.commit();
            core.set_wake_cpu_hint(target);
            drop(sched);
            self.publish_owner_deadline_refresh_reserved(core, target, publication);
        }
        Ok(())
    }

    /// Stops dormant threads from retaining an inactive preferred target.
    ///
    /// Placement is closed before this pass. A producer that sampled the old
    /// target either finishes its runqueue publication before final draining,
    /// making the CPU non-quiescent, or revalidates and selects an online CPU.
    /// This is the active-before-online split used by Linux CPU hotplug around
    /// `task_cpu()` placement.
    fn prepare_thread_targets_for_cpu_offline(
        state: &TaskSystemState,
        root_domain: &RootDomainState,
        cpu: CpuId,
    ) -> bool {
        let is_idle = |id| {
            state
                .cpus
                .iter()
                .any(|registration| registration.remote.idle_thread() == Some(id))
        };
        let fallback_for = |affinity: &CpuSet| {
            state
                .cpus
                .iter()
                .enumerate()
                .map(|(index, registration)| (CpuId::new(index as u32), registration))
                .find(|(candidate, registration)| {
                    *candidate != cpu
                        && root_domain.online.contains(*candidate)
                        && registration.remote.accepts_placement()
                        && affinity.contains(*candidate)
                })
                .map(|(candidate, _)| candidate)
        };

        for record in state.slots.iter().filter_map(|slot| slot.record.as_ref()) {
            if is_idle(record.core.id()) {
                continue;
            }

            let sched = record.sched.lock();
            if sched.lifecycle.state() == ThreadState::Exited {
                continue;
            }
            if Self::is_parked_ktimer_worker(state, cpu, &record.core, &sched) {
                continue;
            }
            if fallback_for(&sched.affinity.affinity).is_none() {
                return false;
            }
            let physically_owned = sched.placement.queued_cpu() == Some(cpu)
                || sched.placement.execution_cpu() == Some(cpu)
                || sched.placement.on_cpu() == Some(cpu)
                || sched.placement.committed_migration_target() == Some(cpu)
                || sched.deadline.bandwidth.reservation_owner() == Some(cpu)
                || record.core.sleep_timer_cpu() == Some(cpu);
            if physically_owned {
                return false;
            }
            let has_other_placement = sched.placement.queued_cpu().is_some()
                || sched.placement.execution_cpu().is_some()
                || sched.placement.on_cpu().is_some()
                || sched.placement.has_pending_migration()
                || sched.deadline.bandwidth.reservation_owner().is_some()
                || record.core.sleep_timer_cpu().is_some();
            if record.core.wake_cpu_hint() == Some(cpu) && has_other_placement {
                return false;
            }
        }

        for record in state.slots.iter().filter_map(|slot| slot.record.as_ref()) {
            if is_idle(record.core.id()) {
                continue;
            }
            let sched = record.sched.lock();
            if Self::is_parked_ktimer_worker(state, cpu, &record.core, &sched) {
                continue;
            }
            drop(sched);
            if record.core.wake_cpu_hint() != Some(cpu) {
                continue;
            }
            let sched = record.sched.lock();
            if sched.lifecycle.state() == ThreadState::Exited {
                continue;
            }
            let Some(fallback) = fallback_for(&sched.affinity.affinity) else {
                return false;
            };
            // Linux deliberately leaves task_cpu() unchanged for blocked
            // tasks; the next wakeup selects from the then-current active mask.
            record.core.set_wake_cpu_hint(fallback);
        }
        true
    }

    fn threads_allow_cpu_offline(
        state: &TaskSystemState,
        root_domain: &RootDomainState,
        cpu: CpuId,
    ) -> bool {
        state
            .slots
            .iter()
            .filter_map(|slot| slot.record.as_ref())
            .all(|record| {
                let id = record.core.id();
                let is_idle = state
                    .cpus
                    .iter()
                    .any(|registration| registration.remote.idle_thread() == Some(id));
                if is_idle {
                    return true;
                }

                let sched = record.sched.lock();
                if sched.lifecycle.state() == ThreadState::Exited {
                    return true;
                }
                if Self::is_parked_ktimer_worker(state, cpu, &record.core, &sched) {
                    return true;
                }
                let has_remaining_destination = (0..state.cpus.len()).any(|index| {
                    let candidate = CpuId::new(index as u32);
                    candidate != cpu
                        && root_domain.online.contains(candidate)
                        && sched.affinity.affinity.contains(candidate)
                });
                let owned_by_cpu = sched.placement.queued_cpu() == Some(cpu)
                    || sched.placement.execution_cpu() == Some(cpu)
                    || sched.placement.on_cpu() == Some(cpu)
                    || sched.placement.committed_migration_target() == Some(cpu)
                    || sched.deadline.bandwidth.reservation_owner() == Some(cpu)
                    || record.core.sleep_timer_cpu() == Some(cpu)
                    || record.core.wake_cpu_hint() == Some(cpu);
                has_remaining_destination && !owned_by_cpu
            })
    }

    /// Linux keeps each `ktimers/%u` task allocated while its CPU is offline.
    /// The fixed task is hotplug-quiescent only after it has parked on its IRQ
    /// event and relinquished every rq, timer, and Deadline ownership record.
    fn is_parked_ktimer_worker(
        state: &TaskSystemState,
        cpu: CpuId,
        core: &ThreadCore,
        sched: &ThreadSchedState,
    ) -> bool {
        state.cpus.get(cpu.as_usize()).is_some_and(|registration| {
            registration.remote.ktimer_worker() == Some(core.id())
                && sched.lifecycle.state() == ThreadState::Blocked
                && sched.placement.queued_cpu().is_none()
                && sched.placement.execution_cpu().is_none()
                && sched.placement.on_cpu().is_none()
                && !sched.placement.has_pending_migration()
                && sched.deadline.bandwidth.reservation_owner().is_none()
                && core.sleep_timer_cpu().is_none()
        })
    }

    /// Installs an idle thread for a CPU; idle is selected only when queues empty.
    pub fn install_idle_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let core = {
            let state = self.state.lock();
            state.cpu_registration(cpu.owner())?;
            Arc::clone(&state.thread_record(thread)?.core)
        };
        self.install_idle_core(cpu.as_mut(), core)
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::{DeadlineFlags, DeadlinePolicy, FairMode, Nice, SchedulePolicy, ThreadSpec};

    fn blocked_thread_fixture() -> (
        Pin<Box<TaskSystem>>,
        Pin<Box<CpuLocal>>,
        Pin<Box<CpuLocal>>,
        ThreadHandle,
    ) {
        let system = Box::pin(TaskSystem::new(TaskSystemConfig::new(2)).unwrap());
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        for cpu in [&mut cpu0, &mut cpu1] {
            system
                .register_idle_thread(
                    cpu.as_mut(),
                    ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
                )
                .unwrap();
            system.bring_cpu_online(cpu.as_mut()).unwrap();
        }

        let sleeper = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(sleeper.id()).unwrap();
        system.enqueue(cpu1.as_mut(), sleeper.id()).unwrap();
        assert_eq!(
            system.schedule(cpu1.as_mut(), None).unwrap().next(),
            sleeper.id()
        );
        system.complete_context_switch(cpu1.as_mut()).unwrap();
        let ParkPrepare::Prepared(mut ticket) =
            system.prepare_park(cpu1.as_mut(), &sleeper).unwrap()
        else {
            panic!("the isolated sleeper must enter the park transaction")
        };
        let ParkCommit::Blocked(decision) = system
            .commit_park(cpu1.as_mut(), &sleeper, &mut ticket)
            .unwrap()
        else {
            panic!("the isolated sleeper cannot race with a notification")
        };
        assert_ne!(decision.next(), sleeper.id());
        system.complete_context_switch(cpu1.as_mut()).unwrap();
        assert_eq!(sleeper.state(), ThreadState::Blocked);
        (system, cpu0, cpu1, sleeper)
    }

    #[test]
    fn deadline_bandwidth_rejects_cpu_capacity_shrink() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        for cpu in [&mut cpu0, &mut cpu1] {
            system
                .register_idle_thread(
                    cpu.as_mut(),
                    ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
                )
                .unwrap();
            system.bring_cpu_online(cpu.as_mut()).unwrap();
        }

        let policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(3, 4, 4, DeadlineFlags::NONE).unwrap());
        let _first = system.create_thread(ThreadSpec::new(policy)).unwrap();
        let _second = system.create_thread(ThreadSpec::new(policy)).unwrap();

        assert_eq!(
            system.take_cpu_offline(cpu1.as_mut()),
            Err(TaskError::DeadlineAdmission),
            "Linux dl_bw_deactivate rejects a CPU-down transition that would overcommit the root \
             domain",
        );
        assert!(
            cpu1.is_online(),
            "a rejected capacity shrink keeps the CPU active"
        );
        assert_eq!(system.online_cpu_count(), 2);
    }

    #[test]
    fn deadline_extra_bandwidth_tracks_cpu_hotplug_topology() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        for cpu in [&mut cpu0, &mut cpu1] {
            system
                .register_idle_thread(
                    cpu.as_mut(),
                    ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
                )
                .unwrap();
        }
        system.bring_cpu_online(cpu0.as_mut()).unwrap();

        let policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(3, 4, 4, DeadlineFlags::NONE).unwrap());
        let _deadline = system.create_thread(ThreadSpec::new(policy)).unwrap();
        assert_eq!(cpu0.remote().deadline_extra_bw_scaled(), 200_000_000);

        system.bring_cpu_online(cpu1.as_mut()).unwrap();
        assert_eq!(cpu0.remote().deadline_extra_bw_scaled(), 575_000_000);
        assert_eq!(cpu1.remote().deadline_extra_bw_scaled(), 575_000_000);

        system.take_cpu_offline(cpu1.as_mut()).unwrap();
        assert_eq!(cpu0.remote().deadline_extra_bw_scaled(), 200_000_000);
        assert_eq!(
            cpu1.remote().deadline_extra_bw_scaled(),
            950_000_000,
            "an offline dl_rq resets to its configured maximum before reuse"
        );
    }

    #[test]
    fn deadline_extra_bandwidth_rounds_each_root_domain_reservation() {
        let system = TaskSystem::new(TaskSystemConfig::new(2)).unwrap();
        let mut cpu0 = system.create_cpu_local(CpuId::new(0)).unwrap();
        let mut cpu1 = system.create_cpu_local(CpuId::new(1)).unwrap();
        for cpu in [&mut cpu0, &mut cpu1] {
            system
                .register_idle_thread(
                    cpu.as_mut(),
                    ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
                )
                .unwrap();
            system.bring_cpu_online(cpu.as_mut()).unwrap();
        }
        let one_nanosecond = SchedulePolicy::deadline(
            DeadlinePolicy::new(1, 1_000_000_000, 1_000_000_000, DeadlineFlags::NONE).unwrap(),
        );
        let first = system
            .create_thread(ThreadSpec::new(one_nanosecond))
            .unwrap();
        let _second = system
            .create_thread(ThreadSpec::new(one_nanosecond))
            .unwrap();

        assert_eq!(cpu0.remote().deadline_extra_bw_scaled(), 950_000_000);
        assert_eq!(cpu1.remote().deadline_extra_bw_scaled(), 950_000_000);
        system.take_cpu_offline(cpu1.as_mut()).unwrap();
        assert_eq!(cpu0.remote().deadline_extra_bw_scaled(), 949_999_998);
        system.bring_cpu_online(cpu1.as_mut()).unwrap();
        assert_eq!(cpu0.remote().deadline_extra_bw_scaled(), 950_000_000);
        assert_eq!(cpu1.remote().deadline_extra_bw_scaled(), 950_000_000);

        system
            .set_thread_policy(
                first.id(),
                SchedulePolicy::deadline(
                    DeadlinePolicy::new(2, 1_000_000_000, 1_000_000_000, DeadlineFlags::NONE)
                        .unwrap(),
                ),
            )
            .unwrap();
        assert_eq!(cpu0.remote().deadline_extra_bw_scaled(), 949_999_999);
        assert_eq!(cpu1.remote().deadline_extra_bw_scaled(), 949_999_999);
    }

    #[test]
    fn detached_deadline_policy_release_updates_root_domain_synchronously() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .register_idle_thread(
                cpu.as_mut(),
                ThreadSpec::new(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
            )
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let deadline = system
            .create_thread(ThreadSpec::new(SchedulePolicy::deadline(
                DeadlinePolicy::new(1, 2, 2, DeadlineFlags::NONE).unwrap(),
            )))
            .unwrap();
        assert_eq!(cpu.remote().deadline_extra_bw_scaled(), 450_000_000);

        system
            .set_thread_policy(deadline.id(), SchedulePolicy::default())
            .unwrap();
        assert_eq!(cpu.remote().deadline_extra_bw_scaled(), 950_000_000);
    }

    #[test]
    fn blocked_thread_keeps_task_cpu_but_publishes_an_online_wake_hint() {
        let (system, _cpu0, mut cpu1, sleeper) = blocked_thread_fixture();
        assert_eq!(sleeper.assigned_cpu(), Some(CpuId::new(1)));

        system.take_cpu_offline(cpu1.as_mut()).unwrap();

        assert_eq!(
            sleeper.assigned_cpu(),
            Some(CpuId::new(1)),
            "Linux leaves task_cpu unchanged while a task is blocked"
        );
        assert_eq!(sleeper.core.wake_cpu_hint(), Some(CpuId::new(0)));
    }

    #[test]
    fn wake_sampled_before_cpu_offline_revalidates_the_target_runqueue() {
        let (system, mut cpu0, mut cpu1, sleeper) = blocked_thread_fixture();
        let wake = sleeper.wake_handle();
        let stale_target = sleeper
            .assigned_cpu()
            .expect("the blocked thread retains its previous direct-wake target");
        assert_eq!(stale_target, CpuId::new(1));

        system.take_cpu_offline(cpu1.as_mut()).unwrap();

        crate::test_runtime::install_task_handles(
            (system.as_ref().get_ref() as *const TaskSystem).expose_provenance(),
            // SAFETY: the test keeps this owner object pinned and clears the
            // runtime handles before resuming direct CpuLocal access.
            (unsafe { Pin::get_unchecked_mut(cpu0.as_mut()) } as *mut CpuLocal).expose_provenance(),
        );
        assert_eq!(
            wake.wake_from_cpu_hint_for_test(stale_target),
            crate::WakeResult::Notified,
            "a wake that sampled the target before CPU-down must not lose that event"
        );
        crate::test_runtime::clear_task_handles();
        assert_eq!(sleeper.state(), ThreadState::Ready);
        assert_eq!(system.snapshot(cpu0.as_ref()).unwrap().runnable(), 1);
    }
}
