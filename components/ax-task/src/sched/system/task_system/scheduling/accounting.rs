//! Accounting under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Charges the current dispatch and reports class budget expiration.
    pub fn charge_current(
        &self,
        cpu: Pin<&mut CpuLocal>,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and all dispatch-tail
        // mutations are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
        if transaction.current().is_none() {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        }
        let charge = transaction.charge_current(runtime_ns, reclaimed_ns);
        transaction.commit();
        Ok(ChargeOutcome {
            slice_expired: charge.slice_expired,
            deadline_overrun: charge.deadline_overrun,
        })
    }

    /// Charges exactly the unaccounted runtime since the current dispatch began
    /// or was last sampled.
    pub fn charge_current_until(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        self.charge_current_until_with_clock(cpu, reclaimed_ns)
            .map(|(charge, _clock, _thread, _rq_observation)| charge)
    }

    pub(crate) fn charge_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
    ) -> Result<
        (
            ChargeOutcome,
            RunQueueClockSnapshot,
            ThreadId,
            SchedulerDeadlineRqObservation,
        ),
        TaskError,
    > {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and all dispatch-tail
        // mutations are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.settle_current(reclaimed_ns);
        let rq_observation = transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
            rq_observation,
        ))
    }

    pub(crate) fn task_tick_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
        tick_ns: u64,
    ) -> Result<
        (
            ChargeOutcome,
            RunQueueClockSnapshot,
            ThreadId,
            SchedulerDeadlineRqObservation,
        ),
        TaskError,
    > {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and all dispatch-tail
        // mutations are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.task_tick_current_until(reclaimed_ns, tick_ns);
        let rq_observation = transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
            rq_observation,
        ))
    }

    pub(crate) fn clock_event_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
    ) -> Result<
        (
            ChargeOutcome,
            RunQueueClockSnapshot,
            ThreadId,
            SchedulerDeadlineRqObservation,
        ),
        TaskError,
    > {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint for the complete accounting transaction.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.clock_event_current_until(reclaimed_ns);
        let rq_observation = transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
            rq_observation,
        ))
    }

    pub(crate) fn task_tick_and_clock_event_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
        tick_ns: u64,
    ) -> Result<
        (
            ChargeOutcome,
            RunQueueClockSnapshot,
            ThreadId,
            SchedulerDeadlineRqObservation,
        ),
        TaskError,
    > {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint for the complete accounting transaction.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.task_tick_and_clock_event_current_until(reclaimed_ns, tick_ns);
        let rq_observation = transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
            rq_observation,
        ))
    }

    /// Reports Linux `!rt_rq_throttled(rq)` for the owner runqueue.
    pub fn rt_run_queue_may_run(&self, cpu: Pin<&mut CpuLocal>) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
        let run_queue = cpu
            .remote()
            .lock_run_queue(RunQueueGuardSource::RtAccounting);
        Ok(!run_queue.rt_is_throttled() || run_queue.has_exempt_rt())
    }
}
