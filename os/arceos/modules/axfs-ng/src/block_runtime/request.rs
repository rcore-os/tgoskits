use alloc::{collections::BTreeMap, vec::Vec};

use rdif_block::{BlkError, RequestId};

use super::DmaBufferGuard;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeRequestId(usize);

impl RuntimeRequestId {
    pub const fn new(id: usize) -> Self {
        Self(id)
    }
}

impl From<RuntimeRequestId> for usize {
    fn from(value: RuntimeRequestId) -> Self {
        value.0
    }
}

pub type RequestKey = RuntimeRequestId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmittedRequest {
    pub queue_id: usize,
    pub request_id: RequestId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestState {
    New,
    Submitted,
    Pending,
    Completing,
    Complete,
    Failed,
    Abandoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollClaim {
    Claimed,
    AlreadyPolling,
    MissingOrComplete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollProgress {
    Pending,
    Repoll,
    Complete,
}

pub struct PendingRequest {
    submitted: SubmittedRequest,
    state: RequestState,
    waiter_task_id: Option<u64>,
    buffer_guard: Option<DmaBufferGuard>,
    result: Option<Result<(), BlkError>>,
    polling: bool,
    repoll: bool,
}

impl PendingRequest {
    pub fn submitted(submitted: SubmittedRequest, buffer_guard: Option<DmaBufferGuard>) -> Self {
        Self {
            submitted,
            state: RequestState::Submitted,
            waiter_task_id: None,
            buffer_guard,
            result: None,
            polling: false,
            repoll: false,
        }
    }

    pub const fn state(&self) -> RequestState {
        self.state
    }

    pub const fn submitted_request(&self) -> SubmittedRequest {
        self.submitted
    }

    pub const fn result(&self) -> Option<Result<(), BlkError>> {
        self.result
    }

    pub const fn holds_buffer_guard(&self) -> bool {
        self.buffer_guard.is_some()
    }

    pub const fn waiter_task_id(&self) -> Option<u64> {
        self.waiter_task_id
    }

    pub fn take_completed_guard(&mut self) -> Option<DmaBufferGuard> {
        if self.result.is_some() {
            self.buffer_guard.take()
        } else {
            None
        }
    }

    fn register_waiter_task(&mut self, task_id: u64) -> bool {
        if self.result.is_some() {
            return true;
        }
        self.waiter_task_id = Some(task_id);
        self.state = RequestState::Pending;
        false
    }

    fn set_completing(&mut self) {
        if !matches!(
            self.state,
            RequestState::Abandoned | RequestState::Complete | RequestState::Failed
        ) {
            self.state = RequestState::Completing;
        }
    }

    fn complete(&mut self, result: Result<(), BlkError>) -> Option<u64> {
        if self.result.is_some() {
            return None;
        }
        let abandoned = matches!(self.state, RequestState::Abandoned);
        self.result = Some(result);
        self.polling = false;
        self.repoll = false;
        self.state = if result.is_ok() {
            RequestState::Complete
        } else {
            RequestState::Failed
        };
        if abandoned {
            self.buffer_guard.take();
        }
        self.waiter_task_id.take()
    }

    fn abandon(&mut self) -> bool {
        self.waiter_task_id.take();
        if self.result.is_some() {
            return true;
        }
        self.state = RequestState::Abandoned;
        false
    }
}

#[derive(Default)]
pub struct PendingTable {
    requests: BTreeMap<RequestKey, PendingRequest>,
    next_runtime_id: usize,
}

impl PendingTable {
    pub const fn new() -> Self {
        Self {
            requests: BTreeMap::new(),
            next_runtime_id: 1,
        }
    }

    pub fn contains_inflight_driver_request(&self, queue_id: usize, request_id: RequestId) -> bool {
        self.requests.values().any(|request| {
            request.result.is_none()
                && request.submitted.queue_id == queue_id
                && request.submitted.request_id == request_id
        })
    }

    pub fn insert_submitted(
        &mut self,
        queue_id: usize,
        request_id: RequestId,
        buffer_guard: Option<DmaBufferGuard>,
    ) -> Result<RequestKey, BlkError> {
        if self.contains_inflight_driver_request(queue_id, request_id) {
            return Err(BlkError::InvalidRequest);
        }
        let key = RuntimeRequestId::new(self.next_runtime_id);
        self.next_runtime_id = self.next_runtime_id.wrapping_add(1).max(1);
        self.requests.insert(
            key,
            PendingRequest::submitted(
                SubmittedRequest {
                    queue_id,
                    request_id,
                },
                buffer_guard,
            ),
        );
        Ok(key)
    }

    pub fn register_waiter_task(
        &mut self,
        key: RequestKey,
        task_id: u64,
    ) -> Option<Result<(), BlkError>> {
        let request = self.requests.get_mut(&key)?;
        if request.register_waiter_task(task_id) {
            request.result
        } else {
            None
        }
    }

    pub fn mark_pending(&mut self, key: RequestKey) {
        if let Some(request) = self.requests.get_mut(&key) {
            request.polling = false;
            request.repoll = false;
            if !matches!(request.state, RequestState::Abandoned) {
                request.state = RequestState::Pending;
            }
        }
    }

    pub fn complete(&mut self, key: RequestKey, result: Result<(), BlkError>) -> Option<u64> {
        self.requests
            .get_mut(&key)
            .and_then(|request| request.complete(result))
    }

    pub fn abandon(&mut self, key: RequestKey) {
        let remove = self
            .requests
            .get_mut(&key)
            .is_some_and(PendingRequest::abandon);
        if remove {
            self.requests.remove(&key);
        }
    }

    pub fn begin_poll(&mut self, key: RequestKey) -> PollClaim {
        let Some(request) = self.requests.get_mut(&key) else {
            return PollClaim::MissingOrComplete;
        };
        if request.result.is_some() {
            return PollClaim::MissingOrComplete;
        }
        if request.polling {
            request.repoll = true;
            return PollClaim::AlreadyPolling;
        }
        request.polling = true;
        request.set_completing();
        PollClaim::Claimed
    }

    pub fn finish_pending_poll(&mut self, key: RequestKey) -> PollProgress {
        let Some(request) = self.requests.get_mut(&key) else {
            return PollProgress::Complete;
        };
        if request.repoll {
            request.repoll = false;
            request.polling = true;
            request.set_completing();
            return PollProgress::Repoll;
        }
        request.polling = false;
        if !matches!(request.state, RequestState::Abandoned) {
            request.state = RequestState::Pending;
        }
        PollProgress::Pending
    }

    pub fn take_completed(
        &mut self,
        key: RequestKey,
    ) -> Option<(Result<(), BlkError>, Option<DmaBufferGuard>)> {
        let request = self.requests.get(&key)?;
        let result = request.result?;
        let mut request = self.requests.remove(&key)?;
        Some((result, request.buffer_guard.take()))
    }

    pub fn request(&self, key: RequestKey) -> Option<&PendingRequest> {
        self.requests.get(&key)
    }

    pub fn result(&self, key: RequestKey) -> Option<Result<(), BlkError>> {
        self.requests.get(&key).and_then(PendingRequest::result)
    }

    pub fn keys_for_queue(&self, queue_id: usize) -> Vec<RequestKey> {
        self.requests
            .iter()
            .filter_map(|(key, request)| {
                (request.submitted.queue_id == queue_id && request.result.is_none()).then_some(*key)
            })
            .collect()
    }

    pub fn matching_driver_keys(&self, queue_id: usize, ids: &[RequestId]) -> Vec<RequestKey> {
        self.requests
            .iter()
            .filter_map(|(key, request)| {
                (request.submitted.queue_id == queue_id
                    && request.result.is_none()
                    && ids.contains(&request.submitted.request_id))
                .then_some(*key)
            })
            .collect()
    }

    pub fn pending_queue_bits(&self) -> u64 {
        let mut bits = 0u64;
        for request in self.requests.values() {
            if request.result.is_none() && request.submitted.queue_id < u64::BITS as usize {
                bits |= 1 << request.submitted.queue_id;
            }
        }
        bits
    }

    pub fn active_keys(&self) -> Vec<RequestKey> {
        self.requests
            .iter()
            .filter_map(|(key, request)| request.result.is_none().then_some(*key))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.requests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }
}
