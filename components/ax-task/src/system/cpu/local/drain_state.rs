use super::*;

#[derive(Debug)]
pub(crate) struct OwnerDrainScratch {
    pub(crate) owner_control_buffer: Vec<InboxMessage>,
    batch_limit: usize,
}

impl OwnerDrainScratch {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            owner_control_buffer: vec![InboxMessage::EMPTY; config.batch_limit()],
            batch_limit: config.batch_limit(),
        }
    }

    pub(crate) const fn batch_limit(&self) -> usize {
        self.batch_limit
    }
}
