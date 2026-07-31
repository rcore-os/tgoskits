use super::*;

#[derive(Debug)]
pub(crate) struct DeadlineClassState {
    /// Stable references to Deadline reservations whose GRUB/CBS state is
    /// owned by this CPU, including blocked non-contending reservations that
    /// are absent from both the current dispatch and the runqueue.
    pub(crate) members: Vec<Arc<ThreadCore>>,
    pub(crate) admitted_bw_scaled: u64,
    pub(crate) running_bw_scaled: u64,
    pub(crate) max_bw_scaled: u64,
}

impl DeadlineClassState {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            members: Vec::with_capacity(config.timer_capacity()),
            admitted_bw_scaled: 0,
            running_bw_scaled: 0,
            max_bw_scaled: u64::from(config.deadline_cap_percent()) * 10_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TaskDeadlinePublicationState {
    pub(super) deadline: Option<MonotonicDeadline>,
    pub(super) deferred_work: bool,
}

#[derive(Debug)]
pub(crate) struct LocalTaskDeadlineState {
    pub(crate) queue: TaskDeadlineQueue,
    pub(crate) expired_buffer: Vec<ExpiredTaskDeadline>,
    pub(crate) expired_count: usize,
    pub(super) generation: u64,
    pub(super) publication: Option<TaskDeadlinePublicationState>,
    #[cfg(test)]
    pub(super) expire_passes: usize,
}

impl LocalTaskDeadlineState {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            queue: TaskDeadlineQueue::new(config.timer_capacity()),
            expired_buffer: vec![ExpiredTaskDeadline::EMPTY; config.batch_limit()],
            expired_count: 0,
            generation: 0,
            publication: None,
            #[cfg(test)]
            expire_passes: 0,
        }
    }
}
