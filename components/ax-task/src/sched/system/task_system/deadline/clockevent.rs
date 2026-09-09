//! Clockevent under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    pub(in crate::sched::system::task_system) fn program_local_timer(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<(), TaskError> {
        let rq_observation = cpu.scheduler_deadline_rq_observation();
        self.program_local_timer_from_rq_observation(cpu.as_mut(), rq_observation, source)
    }

    pub(in crate::sched::system::task_system) fn program_local_timer_from_rq_observation(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        rq_observation: SchedulerDeadlineRqObservation,
        source: SchedulerDeadlineDerivationSource,
    ) -> Result<(), TaskError> {
        let runtime_deadline = cpu.scheduler_runtime_deadline_for_rq_observation(rq_observation);
        if !cpu
            .as_ref()
            .get_ref()
            .can_reuse_scheduler_deadline_for_rq_observation(rq_observation)
            && let Some(update) = cpu
                .as_mut()
                .next_scheduler_deadline_update_if_changed_from_rq_observation(
                    rq_observation,
                    source,
                )?
        {
            task_runtime::publish_scheduler_deadline(update);
        }
        task_runtime::publish_scheduler_runtime_deadline(runtime_deadline);
        Ok(())
    }
}
