use rdif_block::{BlkError, CompletionHint, RequestId};

use super::{DrainEvents, PendingTable, PollClaim, PollProgress, RequestKey, RuntimeDmaBuffer};
use crate::os::{sync::SpinMutex as SpinNoIrq, wake_task};

struct ClaimedQueueBatch {
    queue_id: usize,
    claimed: alloc::vec::Vec<RequestKey>,
    driver_ids: alloc::vec::Vec<RequestId>,
}

pub trait RequestPoller {
    fn poll_request(
        &mut self,
        queue_id: usize,
        request_id: RequestId,
    ) -> Result<PollOutcome, BlkError>;

    fn poll_completions(
        &mut self,
        queue_id: usize,
        request_ids: &[RequestId],
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        for &request_id in request_ids {
            match self.poll_request(queue_id, request_id) {
                Ok(PollOutcome::Pending) => {}
                Ok(PollOutcome::Complete { result, buffer }) => {
                    sink.complete_with_buffer(request_id, result, buffer);
                }
                Err(err) => sink.complete(request_id, Err(err)),
            }
        }
        Ok(())
    }

    fn poll_batch_query_failed(&mut self, queue_id: usize) {
        let _ = queue_id;
    }

    fn completed_request(&mut self, queue_id: usize, task_id: Option<u64>) {
        let _ = queue_id;
        if let Some(task_id) = task_id {
            wake_task(task_id);
        }
    }
}

pub enum PollOutcome {
    Pending,
    Complete {
        result: Result<(), BlkError>,
        buffer: Option<RuntimeDmaBuffer>,
    },
}

impl PollOutcome {
    pub const fn complete(result: Result<(), BlkError>) -> Self {
        Self::Complete {
            result,
            buffer: None,
        }
    }

    pub const fn complete_with_buffer(
        result: Result<(), BlkError>,
        buffer: RuntimeDmaBuffer,
    ) -> Self {
        Self::Complete {
            result,
            buffer: Some(buffer),
        }
    }
}

pub trait CompletionSink {
    fn complete(&mut self, request_id: RequestId, result: Result<(), BlkError>);

    fn complete_with_buffer(
        &mut self,
        request_id: RequestId,
        result: Result<(), BlkError>,
        buffer: Option<RuntimeDmaBuffer>,
    ) {
        let _ = buffer;
        self.complete(request_id, result);
    }
}

pub struct CompletionDrain<'a, P> {
    pending: &'a SpinNoIrq<PendingTable>,
    poller: &'a mut P,
}

impl<'a, P: RequestPoller> CompletionDrain<'a, P> {
    pub const fn new(pending: &'a SpinNoIrq<PendingTable>, poller: &'a mut P) -> Self {
        Self { pending, poller }
    }

    pub fn drain_events(&mut self, events: DrainEvents) -> usize {
        let mut completed = 0;
        for hint in events.hints.iter() {
            completed += self.drain_hint(hint);
        }
        for queue_id in queue_ids_from_bits(events.queue_bits) {
            completed += self.drain_queue(queue_id);
        }
        completed
    }

    pub fn drain_hint(&mut self, hint: CompletionHint) -> usize {
        match hint {
            CompletionHint::Queue { queue_id } => self.drain_queue(queue_id),
            CompletionHint::Request {
                queue_id,
                request_id,
            } => self.poll_matching_driver_requests(queue_id, &[request_id]),
            CompletionHint::Batch { queue_id, ids } => {
                let ids = ids.iter().collect::<alloc::vec::Vec<_>>();
                self.poll_matching_driver_requests(queue_id, &ids)
            }
        }
    }

    pub fn drain_queue(&mut self, queue_id: usize) -> usize {
        let keys = self.pending.lock().keys_for_queue(queue_id);
        self.poll_batch(&keys)
    }

    pub fn poll_keys(&mut self, keys: &[RequestKey]) -> usize {
        self.poll_batch(keys)
    }

    pub fn poll_one(&mut self, key: RequestKey) -> bool {
        if self.pending.lock().begin_poll(key) != PollClaim::Claimed {
            return false;
        }
        self.poll_claimed_one(key)
    }

    fn poll_matching_driver_requests(&mut self, queue_id: usize, ids: &[RequestId]) -> usize {
        let keys = self.matching_driver_keys(queue_id, ids);
        self.poll_batch(&keys)
    }

    fn matching_driver_keys(
        &self,
        queue_id: usize,
        ids: &[RequestId],
    ) -> alloc::vec::Vec<RequestKey> {
        self.pending.lock().matching_driver_keys(queue_id, ids)
    }

    fn poll_batch(&mut self, keys: &[RequestKey]) -> usize {
        let batches = self.claim_poll_batches(keys);
        if batches.is_empty() {
            return 0;
        }

        let mut completed = 0;
        for batch in batches {
            let mut sink = DrainCompletionSink {
                terminal: alloc::vec::Vec::new(),
                claimed: batch
                    .claimed
                    .iter()
                    .copied()
                    .zip(batch.driver_ids.iter().copied())
                    .map(|(key, request_id)| (request_id, key))
                    .collect(),
            };
            let query_failed = self
                .poller
                .poll_completions(batch.queue_id, &batch.driver_ids, &mut sink)
                .is_err();
            let terminal = sink.terminal;
            let terminal_keys = terminal
                .iter()
                .map(|(key, ..)| *key)
                .collect::<alloc::vec::Vec<_>>();
            for (key, result, buffer) in terminal {
                let queue_id = self
                    .pending
                    .lock()
                    .request(key)
                    .map(|request| request.submitted_request().queue_id);
                let task_id = self
                    .pending
                    .lock()
                    .complete_with_buffer(key, result, buffer);
                if let Some(queue_id) = queue_id {
                    self.poller.completed_request(queue_id, task_id);
                }
                completed += 1;
            }
            for key in batch.claimed {
                if terminal_keys.contains(&key) {
                    continue;
                }
                if query_failed {
                    self.release_claimed_poll(key);
                } else {
                    let progress = self.pending.lock().finish_pending_poll(key);
                    match progress {
                        PollProgress::Pending | PollProgress::Complete => {}
                        PollProgress::Repoll => {
                            completed += usize::from(self.poll_claimed_one(key));
                        }
                    }
                }
            }
            if query_failed {
                self.poller.poll_batch_query_failed(batch.queue_id);
            }
        }
        completed
    }

    fn release_claimed_poll(&self, key: RequestKey) {
        while let PollProgress::Repoll = self.pending.lock().finish_pending_poll(key) {}
    }

    fn claim_poll_batches(&self, keys: &[RequestKey]) -> alloc::vec::Vec<ClaimedQueueBatch> {
        let mut batches: alloc::vec::Vec<ClaimedQueueBatch> = alloc::vec::Vec::new();
        let mut pending = self.pending.lock();
        for &key in keys {
            if pending.begin_poll(key) != PollClaim::Claimed {
                continue;
            }
            let submitted = pending
                .request(key)
                .expect("claimed request must remain present")
                .submitted_request();
            if let Some(batch) = batches
                .iter_mut()
                .find(|batch| batch.queue_id == submitted.queue_id)
            {
                batch.claimed.push(key);
                batch.driver_ids.push(submitted.request_id);
            } else {
                let mut batch = ClaimedQueueBatch {
                    queue_id: submitted.queue_id,
                    claimed: alloc::vec::Vec::new(),
                    driver_ids: alloc::vec::Vec::new(),
                };
                batch.claimed.push(key);
                batch.driver_ids.push(submitted.request_id);
                batches.push(batch);
            }
        }
        batches
    }

    fn poll_claimed_one(&mut self, key: RequestKey) -> bool {
        loop {
            let submitted = match self.pending.lock().request(key) {
                Some(request) => request.submitted_request(),
                None => return false,
            };
            let result = self
                .poller
                .poll_request(submitted.queue_id, submitted.request_id);
            match result {
                Ok(PollOutcome::Pending) => match self.pending.lock().finish_pending_poll(key) {
                    PollProgress::Pending | PollProgress::Complete => return false,
                    PollProgress::Repoll => continue,
                },
                Ok(PollOutcome::Complete { result, buffer }) => {
                    let wake = self
                        .pending
                        .lock()
                        .complete_with_buffer(key, result, buffer);
                    self.poller.completed_request(submitted.queue_id, wake);
                    return true;
                }
                Err(err) => {
                    let wake = self.pending.lock().complete(key, Err(err));
                    self.poller.completed_request(submitted.queue_id, wake);
                    return true;
                }
            };
        }
    }
}

struct DrainCompletionSink {
    terminal: alloc::vec::Vec<(RequestKey, Result<(), BlkError>, Option<RuntimeDmaBuffer>)>,
    claimed: alloc::vec::Vec<(RequestId, RequestKey)>,
}

impl CompletionSink for DrainCompletionSink {
    fn complete(&mut self, request_id: RequestId, result: Result<(), BlkError>) {
        self.complete_with_buffer(request_id, result, None);
    }

    fn complete_with_buffer(
        &mut self,
        request_id: RequestId,
        result: Result<(), BlkError>,
        buffer: Option<RuntimeDmaBuffer>,
    ) {
        if let Some((_, runtime_key)) = self
            .claimed
            .iter()
            .copied()
            .find(|(candidate, _)| *candidate == request_id)
        {
            self.terminal.push((runtime_key, result, buffer));
        }
    }
}

fn queue_ids_from_bits(bits: u64) -> impl Iterator<Item = usize> {
    (0..u64::BITS as usize).filter(move |queue_id| bits & (1 << queue_id) != 0)
}
