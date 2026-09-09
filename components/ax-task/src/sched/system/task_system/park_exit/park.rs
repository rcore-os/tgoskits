//! Park under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Publishes `PARKING` after consuming a wake-before-park notification.
    pub fn prepare_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<ParkPrepare, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
        let core = current.runtime_core_arc();
        let placement = core.sched().placement();
        if placement.queued_cpu() != Some(cpu.owner()) || placement.on_cpu() != Some(cpu.owner()) {
            return Err(TaskError::StaleThreadId);
        }
        self.prepare_current_park(core)
    }

    /// Publishes the current task's wait state before its later schedule pass.
    ///
    /// The runtime's current-thread publication is the architecture-context
    /// identity, like Linux `current`. Resumed and fresh task contexts complete
    /// switch tail before calling task code, so this state publication neither
    /// reclaims `CpuLocal` nor repeats switch-tail completion.
    pub(crate) fn prepare_current_park(
        &self,
        current: &ThreadCore,
    ) -> Result<ParkPrepare, TaskError> {
        let core = current;
        let placement = core.sched().placement();
        let queued_cpu = placement.queued_cpu();
        if core.state() != ThreadState::Running
            || queued_cpu.is_none()
            || placement.on_cpu() != queued_cpu
        {
            return Err(TaskError::StaleThreadId);
        }
        if core.take_park_notification() {
            return Ok(ParkPrepare::Notified);
        }
        let generation = core.next_park_generation()?;
        core.transition_state(ThreadState::Parking)?;
        Ok(ParkPrepare::Prepared(ParkTicket::new(
            core.id(),
            generation,
        )))
    }

    /// Cancels a prepared park because an independent grant won the race.
    pub fn cancel_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        token: &mut ParkTicket,
    ) -> Result<(), TaskError> {
        self.cancel_current_park(cpu, current.runtime_core_arc(), token)
    }

    pub(crate) fn cancel_current_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadCore,
        token: &mut ParkTicket,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if token.is_resolved() || current.id() != token.thread() {
            return Err(TaskError::StaleThreadId);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        let core = current;
        if core.park_generation() != token.generation() {
            return Err(TaskError::StaleThreadId);
        }
        let placement = core.sched().placement();
        if core.state() != ThreadState::Parking
            || placement.queued_cpu() != Some(cpu.owner())
            || placement.on_cpu() != Some(cpu.owner())
        {
            return Err(TaskError::StaleThreadId);
        }
        core.transition_state(ThreadState::Running)?;
        cpu.finish_park_preemption(true);
        token.mark_resolved();
        Ok(())
    }
}
