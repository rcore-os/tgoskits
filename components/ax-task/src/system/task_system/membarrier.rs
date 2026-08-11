//! Linux-style per-mm registration and rq target selection.

use super::*;

pub(crate) struct MembarrierRegistrationPlan<'system> {
    address_space: crate::runtime::AddressSpaceHandle,
    registration: MembarrierRegistration,
    state: AddressSpaceMembarrierState,
    targets: MembarrierCpuTargets<'system>,
}

impl MembarrierRegistrationPlan<'_> {
    pub(crate) const fn targets(&self) -> &CpuSet {
        self.targets.cpus()
    }
}

pub(crate) struct MembarrierCpuTargets<'system> {
    cpus: CpuSet,
    _publications: Vec<CpuRemotePublication<'system>>,
}

impl MembarrierCpuTargets<'_> {
    pub(crate) fn new(cpu_count: usize) -> Self {
        Self {
            cpus: CpuSet::empty(cpu_count),
            _publications: Vec::with_capacity(cpu_count),
        }
    }

    pub(crate) const fn cpus(&self) -> &CpuSet {
        &self.cpus
    }
}

#[derive(Clone, Copy)]
pub(crate) enum MembarrierTarget {
    Global,
    GlobalExpedited,
    PrivateExpedited(AddressSpaceMembarrierId),
}

impl TaskSystem {
    pub(crate) fn begin_current_membarrier_registration<'system>(
        &'system self,
        cpu: Pin<&mut CpuLocal>,
        registration: MembarrierRegistration,
        targets: MembarrierCpuTargets<'system>,
    ) -> Result<MembarrierRegistrationPlan<'system>, TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let mut run_queue = cpu.lock_run_queue(RunQueueGuardSource::Membarrier);
        let address_space = run_queue
            .current()
            .map(CurrentDispatch::address_space)
            .filter(|address_space| !address_space.is_none())
            .ok_or(TaskError::InvalidConfiguration)?;
        let state = task_runtime::update_address_space_membarrier_state(
            address_space,
            registration,
            MembarrierRegistrationPhase::Begin,
        );
        let published = run_queue.refresh_membarrier_state();
        if published != state {
            task_runtime::fatal_invariant(0x4d42_0001, state.identity().into_raw());
        }
        drop(run_queue);

        let targets = self.collect_membarrier_targets(
            MembarrierTarget::PrivateExpedited(state.identity()),
            targets,
        );
        Ok(MembarrierRegistrationPlan {
            address_space,
            registration,
            state,
            targets,
        })
    }

    pub(crate) fn complete_membarrier_registration(&self, plan: MembarrierRegistrationPlan<'_>) {
        let completed = task_runtime::update_address_space_membarrier_state(
            plan.address_space,
            plan.registration,
            MembarrierRegistrationPhase::Complete,
        );
        if completed.identity() != plan.state.identity() || !completed.ready(plan.registration) {
            task_runtime::fatal_invariant(0x4d42_0002, plan.state.identity().into_raw());
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn current_membarrier_targets<'system>(
        &'system self,
        cpu: Pin<&mut CpuLocal>,
        target: MembarrierTarget,
        targets: MembarrierCpuTargets<'system>,
    ) -> Result<MembarrierCpuTargets<'system>, crate::MembarrierError> {
        self.ensure_owner_cpu_online(&cpu)?;
        Ok(self.collect_membarrier_targets(target, targets))
    }

    pub(crate) fn current_private_membarrier_target(
        &self,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<MembarrierTarget, crate::MembarrierError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let run_queue = cpu.lock_run_queue(RunQueueGuardSource::Membarrier);
        let address_space = run_queue
            .current()
            .map(CurrentDispatch::address_space)
            .filter(|address_space| !address_space.is_none())
            .ok_or(TaskError::InvalidConfiguration)?;
        let state = task_runtime::address_space_membarrier_state(address_space);
        if !state.ready(MembarrierRegistration::PrivateExpedited) {
            return Err(crate::MembarrierError::NotRegistered);
        }
        Ok(MembarrierTarget::PrivateExpedited(state.identity()))
    }

    pub(crate) fn refresh_current_membarrier_run_queue(
        &self,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        cpu.lock_run_queue(RunQueueGuardSource::Membarrier)
            .refresh_membarrier_state();
        Ok(())
    }

    fn collect_membarrier_targets<'system>(
        &'system self,
        target: MembarrierTarget,
        mut targets: MembarrierCpuTargets<'system>,
    ) -> MembarrierCpuTargets<'system> {
        targets.cpus.clear();
        targets._publications.clear();
        for remote in &self.cpu_remotes {
            let Some(publication) = remote.begin_publication() else {
                continue;
            };
            let state = remote
                .lock_run_queue(RunQueueGuardSource::Membarrier)
                .membarrier_state();
            let selected = match target {
                MembarrierTarget::Global => true,
                MembarrierTarget::GlobalExpedited => {
                    !state.identity().is_none()
                        && state.requested(MembarrierRegistration::GlobalExpedited)
                }
                MembarrierTarget::PrivateExpedited(identity) => state.identity() == identity,
            };
            if selected {
                assert!(targets.cpus.insert(remote.owner()));
                targets._publications.push(publication);
            }
        }
        targets
    }
}
