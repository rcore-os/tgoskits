use super::*;

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
