//! Fixed-capacity staging and completion batches for one hardware queue.

use core::mem::ManuallyDrop;

use rdif_block::{CompletedRequest, CompletionSink};

use super::{HardwareQueueError, MAX_REQUESTS, RequestTag};

pub(super) struct FixedTagQueue {
    tags: [Option<RequestTag>; MAX_REQUESTS],
    head: usize,
    len: usize,
}

impl FixedTagQueue {
    pub(super) const fn new() -> Self {
        Self {
            tags: [None; MAX_REQUESTS],
            head: 0,
            len: 0,
        }
    }

    pub(super) fn push(&mut self, tag: RequestTag) -> Result<(), HardwareQueueError> {
        if self.len == MAX_REQUESTS || self.contains(tag) {
            return Err(HardwareQueueError::Capacity);
        }
        let tail = (self.head + self.len) % MAX_REQUESTS;
        self.tags[tail] = Some(tag);
        self.len += 1;
        Ok(())
    }

    pub(super) fn pop(&mut self) -> Option<RequestTag> {
        if self.len == 0 {
            return None;
        }
        let tag = self.tags[self.head].take();
        self.head = (self.head + 1) % MAX_REQUESTS;
        self.len -= 1;
        tag
    }

    pub(super) fn remove(&mut self, target: RequestTag) -> bool {
        let mut retained = [None; MAX_REQUESTS];
        let mut retained_len = 0;
        let mut removed = false;
        while let Some(tag) = self.pop() {
            if tag == target && !removed {
                removed = true;
            } else {
                retained[retained_len] = Some(tag);
                retained_len += 1;
            }
        }
        self.tags = retained;
        self.head = 0;
        self.len = retained_len;
        removed
    }

    fn contains(&self, target: RequestTag) -> bool {
        (0..self.len).any(|offset| {
            self.tags[(self.head + offset) % MAX_REQUESTS].is_some_and(|tag| tag == target)
        })
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) fn clear(&mut self) {
        self.tags = [None; MAX_REQUESTS];
        self.head = 0;
        self.len = 0;
    }
}

pub(super) struct CompletionBatch {
    entries: [Option<CompletedRequest>; MAX_REQUESTS],
    pub(super) len: usize,
    overflow: Option<CompletedRequest>,
}

impl CompletionBatch {
    pub(super) fn new() -> Self {
        Self {
            entries: [const { None }; MAX_REQUESTS],
            len: 0,
            overflow: None,
        }
    }

    pub(super) fn drain_with<E>(
        &mut self,
        mut publish: impl FnMut(CompletedRequest) -> Result<(), E>,
    ) -> Result<(), E> {
        let initialized = core::mem::replace(&mut self.len, 0);
        let mut first_error = None;
        for index in 0..initialized {
            let completion = self.entries[index]
                .take()
                .expect("completion batch length covers initialized entries");
            if let Err(error) = publish(completion)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) const fn overflowed(&self) -> bool {
        self.overflow.is_some()
    }

    /// Returns the single ownership-bearing completion that violated the
    /// queue's fixed request-capacity contract.
    ///
    /// The caller must transfer this value into the hctx completion quarantine;
    /// it must not be dropped merely because the driver exceeded its batch.
    pub(super) fn take_overflow(&mut self) -> Option<CompletedRequest> {
        self.overflow.take()
    }

    #[cfg(test)]
    pub(super) const fn has_capacity(&self) -> bool {
        self.len < self.entries.len()
    }
}

impl CompletionSink for CompletionBatch {
    fn complete(&mut self, completion: CompletedRequest) {
        if self.len == self.entries.len() {
            if self.overflow.is_none() {
                self.overflow = Some(completion);
                return;
            }

            // The portable queue contract limits one callback to the hctx's 64
            // accepted requests. The first excess owner is retained for the
            // controller poison lane. A second excess value proves the driver
            // fabricated more ownership than the runtime can represent; keep it
            // alive through the fatal invariant instead of running its Drop.
            let _unrepresentable_owner = ManuallyDrop::new(completion);
            panic!("block driver emitted more than one completion beyond hctx capacity");
        }
        self.entries[self.len] = Some(completion);
        self.len += 1;
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum DispatchDisposition {
    Queued,
    Terminal,
    Deferred,
}

pub(super) struct DispatchResult {
    pub(super) disposition: DispatchDisposition,
    completion: Option<CompletedRequest>,
    recovery_error: Option<HardwareQueueError>,
}

impl DispatchResult {
    pub(super) const fn queued(recovery_error: Option<HardwareQueueError>) -> Self {
        Self {
            disposition: DispatchDisposition::Queued,
            completion: None,
            recovery_error,
        }
    }

    pub(super) const fn deferred() -> Self {
        Self {
            disposition: DispatchDisposition::Deferred,
            completion: None,
            recovery_error: None,
        }
    }

    pub(super) fn terminal(
        completion: CompletedRequest,
        recovery_error: Option<HardwareQueueError>,
    ) -> Self {
        Self {
            disposition: DispatchDisposition::Terminal,
            completion: Some(completion),
            recovery_error,
        }
    }

    pub(super) fn take_terminal(
        &mut self,
    ) -> Result<(CompletedRequest, Option<HardwareQueueError>), HardwareQueueError> {
        if self.disposition != DispatchDisposition::Terminal {
            return Err(HardwareQueueError::RequestState);
        }
        Ok((
            self.completion
                .take()
                .ok_or(HardwareQueueError::RequestState)?,
            self.recovery_error.take(),
        ))
    }

    pub(super) fn take_recovery_error(&mut self) -> Option<HardwareQueueError> {
        self.recovery_error.take()
    }
}

#[cfg(test)]
mod tests {
    use rdif_block::{BlkError, OwnedRequest, RequestFlags, RequestId, RequestOp};

    use super::*;

    fn tag(slot: u8, generation: u64) -> RequestTag {
        RequestTag::from_request_id(RequestId::new(generation as usize * 64 + slot as usize))
            .unwrap()
    }

    #[test]
    fn fixed_tag_queue_preserves_fifo_and_supports_rollback_removal() {
        let mut queue = FixedTagQueue::new();
        let first = tag(1, 1);
        let second = tag(2, 1);
        let third = tag(3, 1);
        queue.push(first).unwrap();
        queue.push(second).unwrap();
        queue.push(third).unwrap();

        assert!(queue.remove(second));
        assert_eq!(queue.pop(), Some(first));
        assert_eq!(queue.pop(), Some(third));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn completion_batch_never_allocates_or_grows() {
        let batch = CompletionBatch::new();
        assert_eq!(batch.entries.len(), MAX_REQUESTS);
        assert_eq!(batch.len, 0);
        assert!(batch.has_capacity());
    }

    #[test]
    fn completion_batch_returns_one_typed_overflow_owner_without_dropping_it() {
        let mut batch = CompletionBatch::new();
        for request in 0..=MAX_REQUESTS {
            batch.complete(CompletedRequest::new(
                RequestId::new(request),
                Err(BlkError::Io),
                OwnedRequest {
                    op: RequestOp::Flush,
                    lba: 0,
                    block_count: 0,
                    data: None,
                    flags: RequestFlags::NONE,
                },
            ));
        }

        assert_eq!(batch.len, MAX_REQUESTS);
        assert!(batch.overflowed());
        assert!(!batch.has_capacity());
        assert_eq!(
            batch.take_overflow().map(|completion| completion.id),
            Some(RequestId::new(MAX_REQUESTS))
        );
    }

    #[test]
    fn completion_batch_drains_later_entries_after_one_publish_error() {
        let mut batch = CompletionBatch::new();
        for request in 0..3 {
            batch.complete(CompletedRequest::new(
                RequestId::new(request),
                Err(BlkError::Io),
                OwnedRequest {
                    op: RequestOp::Flush,
                    lba: 0,
                    block_count: 0,
                    data: None,
                    flags: RequestFlags::NONE,
                },
            ));
        }

        let mut observed = 0;
        let result = batch.drain_with(|completion| {
            observed += 1;
            if usize::from(completion.id) == 1 {
                Err("stale")
            } else {
                Ok(())
            }
        });

        assert_eq!(result, Err("stale"));
        assert_eq!(observed, 3);
    }
}
