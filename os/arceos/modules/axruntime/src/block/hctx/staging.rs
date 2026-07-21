//! Fixed-capacity staging and deferred completion notifications for one hctx.

use rdif_block::{CompletedRequest, CompletionSink};

use super::{HardwareQueue, HardwareQueueError, MAX_REQUESTS, RequestTag};

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

    #[cfg(test)]
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

    #[cfg(test)]
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

#[derive(Clone, Copy)]
struct CompletionNotification {
    tag: RequestTag,
    was_inflight: bool,
}

pub(super) struct CompletionDelivery {
    pub(super) completed: usize,
    pub(super) error: Option<HardwareQueueError>,
}

/// Driver-facing sink that transfers ownership immediately but defers wakeups.
///
/// Every completion is installed in the request table or moved into the
/// DMA-proof-gated hctx quarantine before `complete` returns. Only copyable
/// notification facts stay in this object while the portable driver callback
/// owns its mutable queue borrow. [`Drop`] publishes any installed terminal
/// notifications if the callback unwinds, so accepted request ownership cannot
/// become unreachable behind an omitted explicit finish call.
pub(super) struct DeferredCompletionSink<'queue> {
    queue: &'queue HardwareQueue,
    notifications: [Option<CompletionNotification>; MAX_REQUESTS],
    notification_count: usize,
    completed: usize,
    first_error: Option<HardwareQueueError>,
    finished: bool,
}

impl<'queue> DeferredCompletionSink<'queue> {
    pub(super) fn new(queue: &'queue HardwareQueue) -> Self {
        Self {
            queue,
            notifications: [None; MAX_REQUESTS],
            notification_count: 0,
            completed: 0,
            first_error: None,
            finished: false,
        }
    }

    pub(super) fn finish(mut self) -> CompletionDelivery {
        self.finish_notifications();
        self.finished = true;
        CompletionDelivery {
            completed: self.completed,
            error: self.first_error.take(),
        }
    }

    fn remember_error(&mut self, error: HardwareQueueError) {
        if self.first_error.is_none() {
            self.first_error = Some(error);
        }
    }

    fn finish_notifications(&mut self) {
        let initialized = core::mem::replace(&mut self.notification_count, 0);
        for notification in &mut self.notifications[..initialized] {
            let notification = notification
                .take()
                .expect("completion notification count covers initialized entries");
            self.queue
                .finish_installed_completion(notification.tag, notification.was_inflight);
        }
    }
}

impl CompletionSink for DeferredCompletionSink<'_> {
    fn complete(&mut self, completion: CompletedRequest) {
        self.completed = self.completed.saturating_add(1);
        if self.completed > MAX_REQUESTS && self.first_error.is_none() {
            self.first_error = Some(HardwareQueueError::Capacity);
        }

        let tag = match RequestTag::from_request_id(completion.id) {
            Ok(tag) => tag,
            Err(error) => {
                let error = self
                    .queue
                    .retain_failed_completion(error.into(), completion);
                self.remember_error(error);
                return;
            }
        };
        match self.queue.install_completion_for_delivery(tag, completion) {
            Ok(was_inflight) => {
                assert!(
                    self.notification_count < self.notifications.len(),
                    "request table installed more terminal owners than it has slots"
                );
                self.notifications[self.notification_count] =
                    Some(CompletionNotification { tag, was_inflight });
                self.notification_count += 1;
            }
            Err(error) => self.remember_error(error),
        }
    }
}

impl Drop for DeferredCompletionSink<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.finish_notifications();
        }
    }
}

/// Sink for completions that are invalid in a service-drained queue.
pub(super) struct QuarantineCompletionSink<'queue> {
    queue: &'queue HardwareQueue,
    first_error: Option<HardwareQueueError>,
}

impl<'queue> QuarantineCompletionSink<'queue> {
    pub(super) const fn new(queue: &'queue HardwareQueue) -> Self {
        Self {
            queue,
            first_error: None,
        }
    }

    pub(super) fn finish(mut self) -> Result<(), HardwareQueueError> {
        self.first_error.take().map_or(Ok(()), Err)
    }
}

impl CompletionSink for QuarantineCompletionSink<'_> {
    fn complete(&mut self, completion: CompletedRequest) {
        let error = self
            .queue
            .retain_failed_completion(HardwareQueueError::StaleCompletion, completion);
        if self.first_error.is_none() {
            self.first_error = Some(error);
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum DispatchDisposition {
    Queued,
    Terminal,
}

pub(super) struct DispatchResult {
    pub(super) disposition: DispatchDisposition,
    completion: Option<CompletedRequest>,
    recovery_error: Option<HardwareQueueError>,
}

impl DispatchResult {
    pub(super) const fn queued() -> Self {
        Self {
            disposition: DispatchDisposition::Queued,
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
    use rdif_block::RequestId;

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
}
