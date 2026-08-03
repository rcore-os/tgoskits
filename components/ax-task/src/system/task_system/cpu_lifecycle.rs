//! CPU-local scheduler allocation and online publication.

use super::*;

impl TaskSystem {
    /// Allocates one pinned CPU-local scheduler object without publishing it.
    pub fn create_cpu_local(
        &self,
        cpu: CpuId,
    ) -> Result<Pin<alloc::boxed::Box<CpuLocal>>, TaskError> {
        let remote = Arc::clone(&self.state.lock().cpu_registration(cpu)?.remote);
        Ok(CpuLocal::create(cpu, self.config, remote))
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
    pub fn bring_cpu_online(&self, cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        self.bring_cpu_online_at(cpu, task_runtime::monotonic_ns())
    }

    /// Completes CPU registration at `now_ns` and publishes it online.
    ///
    /// The explicit clock sample keeps deterministic scheduler models and OS
    /// runtimes on the same absolute monotonic time base. In particular, the
    /// first fair-balance deadline is one interval after online publication,
    /// rather than one interval after an unrelated zero epoch.
    pub fn bring_cpu_online_at(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let _irq = IrqScope::enter();
        self.ensure_owner_cpu_context(&cpu)?;
        let id = cpu.owner();
        let mut state = self.state.lock();
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
                (matches!(sched.policy.applied, SchedulePolicy::Deadline(_))
                    || matches!(sched.policy.requested, SchedulePolicy::Deadline(_)))
                    && !sched.placement.affinity.contains(id)
            })
        {
            return Err(TaskError::DeadlineAffinity);
        }
        ensure_runtime_success(task_runtime::prepare_cpu_online(RuntimeCpuId::new(
            id.as_u32(),
        )))?;
        cpu.as_mut()
            .reset_fair_balance(now_ns, self.config.balance_interval_ns());
        let online_count = state
            .online_cpu_count()
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        self.topology_sequence.write_begin();
        assert!(
            root_domain.online.insert(id),
            "validated offline CPU must be absent from the root domain"
        );
        state.deadline_admission.set_online_cpus(online_count);
        self.online_count.store(online_count, Ordering::Release);
        assert!(
            cpu.as_ref().get_ref().remote().mark_online(),
            "validated offline CPU must accept final publication"
        );
        self.topology_sequence.write_end();
        Ok(())
    }

    /// Removes a quiescent owner CPU from placement and remote publication.
    ///
    /// The caller must first migrate or retire every non-idle thread, cancel
    /// local task deadlines, and consume the CPU's scheduler IPI. The packed
    /// remote lifecycle closes publication only when its active publisher count
    /// is zero, so a successful transition cannot strand an inbox node between
    /// queue insertion and its doorbell.
    pub fn take_cpu_offline(&self, cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let _irq = IrqScope::enter();
        let id = cpu.owner();
        let mut state = self.state.lock();
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
        if state.online_cpu_count() <= 1 {
            return Err(TaskError::LastOnlineCpu(id.as_u32()));
        }

        self.topology_sequence.write_begin();
        let result = if !remote.try_deactivate() {
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
        } else if !root_domain.online.remove(id) {
            remote.cancel_draining();
            Err(TaskError::InvalidConfiguration)
        } else {
            let online_count = state.online_cpu_count();
            state.deadline_admission.set_online_cpus(online_count);
            self.online_count.store(online_count, Ordering::Release);
            remote.finish_offline();
            Ok(())
        };
        self.topology_sequence.write_end();
        result
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
            let id = record.core.id();
            let is_idle = state
                .cpus
                .iter()
                .any(|registration| registration.remote.idle_thread() == Some(id));
            if is_idle {
                continue;
            }

            let sched = record.sched.lock();
            if sched.lifecycle.state() == ThreadState::Exited {
                continue;
            }
            if fallback_for(&sched.placement.affinity).is_none() {
                return false;
            }
            let physically_owned = sched.placement.queued_cpu() == Some(cpu)
                || sched.placement.running_cpu() == Some(cpu)
                || sched.placement.on_cpu() == Some(cpu)
                || sched.placement.migration_target() == Some(cpu)
                || sched.deadline.bandwidth_cpu == Some(cpu)
                || record.core.sleep_timer_cpu() == Some(cpu);
            if physically_owned {
                return false;
            }
            let has_other_placement = sched.placement.queued_cpu().is_some()
                || sched.placement.running_cpu().is_some()
                || sched.placement.on_cpu().is_some()
                || sched.placement.migration_target().is_some()
                || sched.deadline.bandwidth_cpu.is_some()
                || record.core.sleep_timer_cpu().is_some();
            if record.core.target_cpu() == Some(cpu) && has_other_placement {
                return false;
            }
        }

        for record in state.slots.iter().filter_map(|slot| slot.record.as_ref()) {
            if record.core.target_cpu() != Some(cpu) {
                continue;
            }
            let sched = record.sched.lock();
            if sched.lifecycle.state() == ThreadState::Exited {
                continue;
            }
            let Some(fallback) = fallback_for(&sched.placement.affinity) else {
                return false;
            };
            record.core.set_target_cpu(fallback);
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
                let has_remaining_destination = (0..state.cpus.len()).any(|index| {
                    let candidate = CpuId::new(index as u32);
                    candidate != cpu
                        && root_domain.online.contains(candidate)
                        && sched.placement.affinity.contains(candidate)
                });
                let owned_by_cpu = sched.placement.queued_cpu() == Some(cpu)
                    || sched.placement.running_cpu() == Some(cpu)
                    || sched.placement.on_cpu() == Some(cpu)
                    || sched.placement.migration_target() == Some(cpu)
                    || sched.deadline.bandwidth_cpu == Some(cpu)
                    || record.core.sleep_timer_cpu() == Some(cpu)
                    || record.core.target_cpu() == Some(cpu);
                has_remaining_destination && !owned_by_cpu
            })
    }

    /// Installs an idle thread for a CPU; idle is selected only when queues empty.
    pub fn install_idle_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let state = self.state.lock();
        state.cpu_registration(cpu.owner())?;
        let core = Arc::clone(&state.thread_record(thread)?.core);
        cpu.as_mut().set_idle(thread, core);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::{FairMode, Nice, ThreadSpec};

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
        system.enqueue(cpu1.as_mut(), sleeper.id(), 0).unwrap();
        assert_eq!(
            system.schedule(cpu1.as_mut(), 0).unwrap().next(),
            sleeper.id()
        );
        system.complete_context_switch(cpu1.as_mut()).unwrap();
        assert_ne!(
            system.block_current(cpu1.as_mut(), 0).unwrap().next(),
            sleeper.id()
        );
        system.complete_context_switch(cpu1.as_mut()).unwrap();
        assert_eq!(sleeper.state(), ThreadState::Blocked);
        (system, cpu0, cpu1, sleeper)
    }

    #[test]
    fn blocked_thread_target_is_redirected_before_cpu_offline() {
        let (system, _cpu0, mut cpu1, sleeper) = blocked_thread_fixture();
        let wake = sleeper.wake_handle();
        assert_eq!(wake.target_cpu(), Some(CpuId::new(1)));

        system.take_cpu_offline(cpu1.as_mut()).unwrap();

        assert_eq!(
            wake.target_cpu(),
            Some(CpuId::new(0)),
            "hotplug preparation must publish an online target before closing the old CPU"
        );
    }

    #[test]
    fn wake_sampled_before_cpu_offline_revalidates_the_target_runqueue() {
        let (system, mut cpu0, mut cpu1, sleeper) = blocked_thread_fixture();
        let wake = sleeper.wake_handle();
        let stale_target = wake
            .target_cpu()
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
            wake.wake_from_target_snapshot_for_test(stale_target),
            crate::WakeResult::Notified,
            "a wake that sampled the target before CPU-down must not lose that event"
        );
        crate::test_runtime::clear_task_handles();
        assert_eq!(sleeper.state(), ThreadState::Ready);
        assert_eq!(system.snapshot(cpu0.as_ref()).unwrap().runnable(), 1);
    }
}
