//! Idle under the owning scheduler transaction.

use super::*;

impl CpuRunQueueState {
    pub(crate) fn install_idle(
        &mut self,
        core: Arc<ThreadCore>,
        active: ActiveSchedulingState,
        metadata: RqTaskMetadata,
        rt_quota_exempt: bool,
    ) {
        if !matches!(
            active.policy(),
            SchedulePolicy::Fair {
                mode: FairMode::Idle,
                ..
            }
        ) {
            task_runtime::fatal_invariant(0x5251_0003, core.id().as_u64() as usize);
        }
        let idle = IdleRqTask {
            core,
            active: Some(active),
            metadata,
            rt_quota_exempt,
        };
        if self.idle.replace(idle).is_some() {
            task_runtime::fatal_invariant(0x5251_0002, self.owner.as_u32() as usize);
        }
    }

    pub(crate) fn idle(&self) -> Option<ThreadId> {
        self.idle.as_ref().map(|idle| idle.core.id())
    }

    pub(crate) fn take_idle_schedule(
        &mut self,
    ) -> Option<(Arc<ThreadCore>, ActiveSchedulingState, RqTaskMetadata, bool)> {
        let idle = self.idle.as_mut()?;
        Some((
            Arc::clone(&idle.core),
            idle.active
                .take()
                .expect("idle schedule cannot be current on two CPUs"),
            idle.metadata.clone(),
            idle.rt_quota_exempt,
        ))
    }

    pub(crate) fn return_idle_schedule(
        &mut self,
        thread: ThreadId,
        active: ActiveSchedulingState,
    ) -> Result<(), TaskError> {
        let idle = self.idle.as_mut().ok_or(TaskError::InvalidConfiguration)?;
        if idle.core.id() != thread || idle.active.replace(active).is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(())
    }

    pub(crate) fn has_exempt_rt(&self) -> bool {
        self.queue.has_exempt_rt()
    }

    pub(crate) const fn rt_is_throttled(&self) -> bool {
        self.rt_throttled
    }

    pub(crate) fn set_rt_throttled(&mut self, throttled: bool) -> bool {
        let changed = self.rt_throttled != throttled;
        self.rt_throttled = throttled;
        changed
    }

    pub(crate) fn has_runnable_rt(&self) -> bool {
        // RT current remains linked in the active priority array, so the
        // class-owned index already includes both queued and running RT work.
        self.queue.has_rt()
    }

    pub(crate) fn highest_rt_priority_including_current(&self) -> Option<u8> {
        self.highest_rt_priority()
    }

    pub(crate) fn earliest_deadline_including_current(&self) -> Option<u64> {
        // Deadline current remains linked in the augmented EDF tree.
        self.earliest_deadline_ns()
    }
}
