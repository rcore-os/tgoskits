use rdif_block::{BlkError, CompletionHint, RequestId, RequestStatus};

use super::{DrainEvents, PendingTable, PollClaim, PollProgress, RequestKey};
use crate::os::{sync::IrqMutex as SpinNoIrq, wake_task};

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
    ) -> Result<RequestStatus, BlkError>;

    fn poll_completions(
        &mut self,
        queue_id: usize,
        request_ids: &[RequestId],
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        for &request_id in request_ids {
            match self.poll_request(queue_id, request_id) {
                Ok(RequestStatus::Pending) => {}
                Ok(RequestStatus::Complete) => sink.complete(request_id, Ok(())),
                Err(err) => sink.complete(request_id, Err(err)),
            }
        }
        Ok(())
    }
}

pub trait CompletionSink {
    fn complete(&mut self, request_id: RequestId, result: Result<(), BlkError>);
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
                pending: self.pending,
                completed: 0,
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
            let terminal = sink.terminal.clone();
            completed += sink.completed;
            for key in batch.claimed {
                if terminal.contains(&key) {
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
            let wake = match result {
                Ok(RequestStatus::Pending) => match self.pending.lock().finish_pending_poll(key) {
                    PollProgress::Pending => None,
                    PollProgress::Repoll => continue,
                    PollProgress::Complete => None,
                },
                Ok(RequestStatus::Complete) => self.pending.lock().complete(key, Ok(())),
                Err(err) => self.pending.lock().complete(key, Err(err)),
            };
            if let Some(task_id) = wake {
                wake_task(task_id);
            }
            return !matches!(result, Ok(RequestStatus::Pending));
        }
    }
}

struct DrainCompletionSink<'a> {
    pending: &'a SpinNoIrq<PendingTable>,
    completed: usize,
    terminal: alloc::vec::Vec<RequestKey>,
    claimed: alloc::vec::Vec<(RequestId, RequestKey)>,
}

impl CompletionSink for DrainCompletionSink<'_> {
    fn complete(&mut self, request_id: RequestId, result: Result<(), BlkError>) {
        if let Some((_, runtime_key)) = self
            .claimed
            .iter()
            .copied()
            .find(|(candidate, _)| *candidate == request_id)
        {
            self.complete_runtime(runtime_key, result);
        }
    }
}

impl DrainCompletionSink<'_> {
    fn complete_runtime(&mut self, runtime_key: RequestKey, result: Result<(), BlkError>) {
        let token = self.pending.lock().complete(runtime_key, result);
        if let Some(task_id) = token {
            wake_task(task_id);
        }
        self.terminal.push(runtime_key);
        self.completed += 1;
    }
}

fn queue_ids_from_bits(bits: u64) -> impl Iterator<Item = usize> {
    (0..u64::BITS as usize).filter(move |queue_id| bits & (1 << queue_id) != 0)
}
