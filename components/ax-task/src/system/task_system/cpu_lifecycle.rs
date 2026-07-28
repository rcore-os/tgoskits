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

    /// Returns the stable remote-publication endpoint of an online CPU.
    pub fn cpu_remote(&self, cpu: CpuId) -> Option<&CpuRemote> {
        self.cpu_remotes
            .get(cpu.as_usize())
            .map(Arc::as_ref)
            .filter(|remote| remote.is_online())
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
        self.ensure_owner_cpu_context(&cpu)?;
        let id = cpu.owner();
        let mut state = self.state.lock();
        let mut root_domain = self.root_domain.lock();
        let registration = state.cpu_registration(id)?;
        if registration.online || cpu.is_online() {
            return Err(TaskError::CpuAlreadyOnline(id.as_u32()));
        }
        if !Arc::ptr_eq(&registration.remote, cpu.remote()) {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        if state
            .slots
            .iter()
            .filter_map(|slot| slot.record.as_ref())
            .any(|record| {
                let sched = record.sched.lock();
                (matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
                    || matches!(sched.base_policy, SchedulePolicy::Deadline(_)))
                    && !sched.affinity.contains(id)
            })
        {
            return Err(TaskError::DeadlineAffinity);
        }
        self.topology_sequence.write_begin();
        state.cpu_registration_mut(id)?.online = true;
        cpu.as_mut()
            .reset_fair_balance(now_ns, self.config.balance_interval_ns());
        cpu.as_ref().get_ref().remote().mark_online();
        root_domain.online.insert(id);
        let online_count = state.online_cpu_count();
        state.deadline_admission.set_online_cpus(online_count);
        self.online_count.store(online_count, Ordering::Release);
        self.topology_sequence.write_end();
        Ok(())
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
