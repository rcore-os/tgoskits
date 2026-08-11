//! IRQ-safe per-CPU task-deadline base.
//!
//! Linux hrtimer bases are per CPU, but their raw lock is reachable from a
//! remote `task_rq_lock()` migration transaction. Keeping this state below
//! [`CpuRemote`] provides the same ownership boundary: the local timer IRQ and
//! soft-timer worker remain the only consumers, while a remote wake migration
//! may cancel an old registration before moving its rq bandwidth.

use super::*;

/// Typed reason for entering the per-CPU task-deadline base.
///
/// Each reason maps to one qperf IRQ-ticket category, so the aggregate keeps
/// one counter update per existing lock acquisition rather than adding a
/// second diagnostic operation to the hot path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeadlineBaseGuardSource {
    Observation,
    Publication,
    Registration,
    HardExpiry,
    SoftExpiry,
    Lifecycle,
    #[cfg(test)]
    TestInspection,
}

impl DeadlineBaseGuardSource {
    pub(crate) const fn irq_guard_source(self) -> crate::runtime::IrqGuardSource {
        match self {
            Self::Observation => crate::runtime::IrqGuardSource::CpuDeadlineObservationTicket,
            Self::Publication => crate::runtime::IrqGuardSource::CpuDeadlinePublicationTicket,
            Self::Registration => crate::runtime::IrqGuardSource::CpuDeadlineRegistrationTicket,
            Self::HardExpiry => crate::runtime::IrqGuardSource::CpuDeadlineHardExpiryTicket,
            Self::SoftExpiry => crate::runtime::IrqGuardSource::CpuDeadlineSoftExpiryTicket,
            Self::Lifecycle => crate::runtime::IrqGuardSource::CpuDeadlineLifecycleTicket,
            #[cfg(test)]
            Self::TestInspection => crate::runtime::IrqGuardSource::CpuDeadlineLifecycleTicket,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchedulerDeadlinePublicationState {
    pub(crate) deadline: Option<MonotonicDeadline>,
}

#[derive(Debug)]
pub(crate) struct CpuDeadlineState {
    pub(crate) queue: TaskDeadlineQueue,
    pub(crate) expired_buffer: Vec<ExpiredTaskDeadline>,
    pub(crate) expired_count: usize,
    /// Mirrors Linux `hrtimer_cpu_base::softirq_activated`.
    ///
    /// A due queue head does not set this bit. Only the hard clockevent path
    /// may transfer progress ownership to `ktimers/%u`; the worker clears the
    /// bit after draining every due and buffered soft expiry.
    pub(crate) softirq_activated: bool,
    pub(crate) generation: u64,
    pub(crate) publication: Option<SchedulerDeadlinePublicationState>,
    #[cfg(test)]
    pub(crate) expire_passes: usize,
}

impl CpuDeadlineState {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            queue: TaskDeadlineQueue::new(config.thread_capacity()),
            expired_buffer: vec![ExpiredTaskDeadline::EMPTY; config.batch_limit()],
            expired_count: 0,
            softirq_activated: false,
            generation: 0,
            publication: None,
            #[cfg(test)]
            expire_passes: 0,
        }
    }

    pub(crate) fn peek_buffered_expiration(&self) -> Option<ExpiredTaskDeadline> {
        self.expired_buffer[..self.expired_count].first().copied()
    }

    pub(crate) fn take_buffered_expiration(
        &mut self,
        registration: &TaskDeadlineRegistration,
    ) -> Option<ExpiredTaskDeadline> {
        self.take_buffered_expiration_if(|event| {
            event.thread() == Some(registration.thread())
                && event.token() == registration.token()
                && event.deadline() == Some(registration.deadline())
                && event.kind() == Some(registration.kind())
        })
    }

    pub(crate) fn take_buffered_event(
        &mut self,
        event: ExpiredTaskDeadline,
    ) -> Option<ExpiredTaskDeadline> {
        self.take_buffered_expiration_if(|candidate| candidate == event)
    }

    fn take_buffered_expiration_if(
        &mut self,
        matches: impl Fn(ExpiredTaskDeadline) -> bool,
    ) -> Option<ExpiredTaskDeadline> {
        let index = self.expired_buffer[..self.expired_count]
            .iter()
            .copied()
            .position(matches)?;
        let event = self.expired_buffer[index];
        self.expired_buffer
            .copy_within(index + 1..self.expired_count, index);
        self.expired_count -= 1;
        self.expired_buffer[self.expired_count] = ExpiredTaskDeadline::EMPTY;
        Some(event)
    }
}
