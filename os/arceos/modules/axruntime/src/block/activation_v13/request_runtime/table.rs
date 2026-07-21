//! Generation-bearing request ownership for one v0.13 ownership domain.

use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kspin::SpinNoPreempt;
use rdif_block::{
    BlkError, CompletedRequest, DriverDeviceKey, OwnedRequest, RequestId, UnacceptedRequest,
};

use crate::task::{TaskError, WaitQueue};

/// Keeps one bounded software-staging generation beyond every hardware tag.
pub(super) const REQUEST_TABLE_STAGING_FACTOR: usize = 2;

/// One domain supports all hardware tags plus its bounded staging generation.
pub(super) const MAX_DOMAIN_REQUESTS: usize = rdif_block::MAX_CONTROLLER_QUEUES
    * super::super::MAX_HARDWARE_CREDITS
    * REQUEST_TABLE_STAGING_FACTOR;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RequestToken {
    pub(super) id: RequestId,
    pub(super) slot: usize,
    pub(super) generation: usize,
}

impl RequestToken {
    pub(super) const fn id(self) -> RequestId {
        self.id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestPhase {
    Staged,
    InFlight,
    Terminal,
}

struct RequestRecord {
    generation: usize,
    queue_id: usize,
    driver_device: DriverDeviceKey,
    deadline_ns: Option<u64>,
    phase: RequestPhase,
    ownership: RequestOwnership,
}

enum RequestOwnership {
    Runtime(OwnedRequest),
    Driver,
    Completed(CompletedRequest),
}

struct RequestSlot {
    generation: usize,
    record: Option<RequestRecord>,
}

struct SharedRequestSlot {
    record: SpinNoPreempt<RequestSlot>,
    completion_wait: WaitQueue,
}

impl SharedRequestSlot {
    fn new() -> Self {
        Self {
            record: SpinNoPreempt::new(RequestSlot {
                generation: 0,
                record: None,
            }),
            completion_wait: WaitQueue::new(),
        }
    }
}

pub(super) struct DomainRequestTable {
    slots: Box<[SharedRequestSlot]>,
    cursor: AtomicUsize,
    inflight: AtomicUsize,
}

pub(super) struct DispatchPermit {
    token: RequestToken,
}

impl DispatchPermit {
    pub(super) const fn accept(self) {}
}

pub(super) struct DispatchRequest {
    pub(super) permit: DispatchPermit,
    pub(super) id: RequestId,
    pub(super) queue_id: usize,
    pub(super) driver_device: DriverDeviceKey,
    pub(super) request: OwnedRequest,
}

pub(super) struct InstalledCompletion {
    pub(super) queue_id: usize,
    token: RequestToken,
}

#[derive(Debug)]
pub(super) struct RequestReservationFailure {
    pub(super) error: RequestTableError,
    pub(super) request: OwnedRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum RequestTableError {
    #[error("v0.13 request-table capacity is outside 1..={MAX_DOMAIN_REQUESTS}")]
    InvalidCapacity,
    #[error("v0.13 request table has no free generation-bearing slot")]
    Exhausted,
    #[error("v0.13 request identity belongs to another table generation")]
    Stale,
    #[error("v0.13 request transition violated the ownership protocol")]
    InvalidTransition,
    #[error("v0.13 driver returned a different request identity")]
    DriverIdentityMismatch,
    #[error("v0.13 request owner is no longer available")]
    OwnerUnavailable,
    #[error(transparent)]
    Task(#[from] TaskError),
}

impl DomainRequestTable {
    pub(super) fn new(capacity: usize) -> Result<Self, RequestTableError> {
        if capacity == 0 || capacity > MAX_DOMAIN_REQUESTS {
            return Err(RequestTableError::InvalidCapacity);
        }
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, SharedRequestSlot::new);
        Ok(Self {
            slots: slots.into_boxed_slice(),
            cursor: AtomicUsize::new(0),
            inflight: AtomicUsize::new(0),
        })
    }

    pub(super) fn reserve(
        &self,
        queue_id: usize,
        driver_device: DriverDeviceKey,
        request: OwnedRequest,
    ) -> Result<RequestToken, RequestReservationFailure> {
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % self.slots.len();
        let mut request = Some(request);
        for offset in 0..self.slots.len() {
            let slot_index = (start + offset) % self.slots.len();
            let slot = &self.slots[slot_index];
            let mut stored = slot.record.lock();
            if stored.record.is_some() {
                continue;
            }
            let Some(generation) = stored.generation.checked_add(1) else {
                continue;
            };
            let Some(id) = encode_request_id(slot_index, generation, self.slots.len()) else {
                continue;
            };
            stored.generation = generation;
            stored.record = Some(RequestRecord {
                generation,
                queue_id,
                driver_device,
                deadline_ns: None,
                phase: RequestPhase::Staged,
                ownership: RequestOwnership::Runtime(
                    request
                        .take()
                        .expect("one reservation consumes request ownership exactly once"),
                ),
            });
            return Ok(RequestToken {
                id,
                slot: slot_index,
                generation,
            });
        }
        Err(RequestReservationFailure {
            error: RequestTableError::Exhausted,
            request: request.expect("an exhausted request table retains request ownership"),
        })
    }

    /// Publishes driver ownership, deadline, and inflight accounting before
    /// the portable submit operation may expose a descriptor or doorbell.
    pub(super) fn begin_dispatch(
        &self,
        token: RequestToken,
        deadline_ns: u64,
    ) -> Result<DispatchRequest, RequestTableError> {
        let mut stored = self.slot(token)?.record.lock();
        let record = checked_record_mut(&mut stored, token)?;
        if record.phase != RequestPhase::Staged {
            return Err(RequestTableError::InvalidTransition);
        }
        let ownership = core::mem::replace(&mut record.ownership, RequestOwnership::Driver);
        let RequestOwnership::Runtime(request) = ownership else {
            record.ownership = ownership;
            return Err(RequestTableError::InvalidTransition);
        };
        record.phase = RequestPhase::InFlight;
        record.deadline_ns = Some(deadline_ns);
        let queue_id = record.queue_id;
        let driver_device = record.driver_device;
        self.inflight.fetch_add(1, Ordering::Release);
        Ok(DispatchRequest {
            permit: DispatchPermit { token },
            id: token.id,
            queue_id,
            driver_device,
            request,
        })
    }

    /// Rolls back only with the driver's linear hardware-not-visible proof.
    pub(super) fn restore_unaccepted(
        &self,
        permit: DispatchPermit,
        unaccepted: UnacceptedRequest,
    ) -> Result<(BlkError, OwnedRequest), RequestTableError> {
        let (returned_id, error, request, _hardware_not_visible) = unaccepted.into_parts();
        if returned_id != permit.token.id {
            return Err(RequestTableError::DriverIdentityMismatch);
        }
        let mut stored = self.slot(permit.token)?.record.lock();
        let record = checked_record_mut(&mut stored, permit.token)?;
        if record.phase != RequestPhase::InFlight
            || !matches!(record.ownership, RequestOwnership::Driver)
        {
            return Err(RequestTableError::InvalidTransition);
        }
        record.phase = RequestPhase::Staged;
        record.deadline_ns = None;
        record.ownership = RequestOwnership::Runtime(request);
        self.release_inflight();
        let ownership = core::mem::replace(&mut record.ownership, RequestOwnership::Driver);
        let RequestOwnership::Runtime(request) = ownership else {
            unreachable!("restored driver rejection returned runtime ownership");
        };
        Ok((error, request))
    }

    pub(super) fn finish_unaccepted(
        &self,
        token: RequestToken,
        error: BlkError,
        request: OwnedRequest,
    ) -> Result<(), RequestTableError> {
        let mut stored = self.slot(token)?.record.lock();
        let record = checked_record_mut(&mut stored, token)?;
        if record.phase != RequestPhase::Staged
            || !matches!(record.ownership, RequestOwnership::Driver)
        {
            return Err(RequestTableError::InvalidTransition);
        }
        record.phase = RequestPhase::Terminal;
        record.ownership =
            RequestOwnership::Completed(CompletedRequest::new(token.id, Err(error), request));
        drop(stored);
        self.slots[token.slot].completion_wait.notify_all();
        Ok(())
    }

    pub(super) fn install_completion(
        &self,
        completion: CompletedRequest,
    ) -> Result<InstalledCompletion, (RequestTableError, CompletedRequest)> {
        let token = match self.decode(completion.id) {
            Ok(token) => token,
            Err(error) => return Err((error, completion)),
        };
        let mut stored = self.slots[token.slot].record.lock();
        let record = match checked_record_mut(&mut stored, token) {
            Ok(record) => record,
            Err(error) => return Err((error, completion)),
        };
        if record.phase != RequestPhase::InFlight
            || !matches!(record.ownership, RequestOwnership::Driver)
        {
            return Err((RequestTableError::InvalidTransition, completion));
        }
        let queue_id = record.queue_id;
        record.phase = RequestPhase::Terminal;
        record.deadline_ns = None;
        record.ownership = RequestOwnership::Completed(completion);
        self.release_inflight();
        Ok(InstalledCompletion { queue_id, token })
    }

    pub(super) fn notify_completion(&self, installed: InstalledCompletion) {
        self.slots[installed.token.slot]
            .completion_wait
            .notify_all();
    }

    pub(super) fn wait_and_take(
        &self,
        token: RequestToken,
        owner_live: impl Fn() -> bool,
    ) -> Result<CompletedRequest, RequestTableError> {
        let slot = self.slot(token)?;
        slot.completion_wait
            .try_wait_until(|| completion_ready(slot, token) || !owner_live())?;
        let mut stored = slot.record.lock();
        let record = checked_record_mut(&mut stored, token)?;
        if record.phase != RequestPhase::Terminal {
            return Err(RequestTableError::OwnerUnavailable);
        }
        let ownership = core::mem::replace(&mut record.ownership, RequestOwnership::Driver);
        let RequestOwnership::Completed(completion) = ownership else {
            record.ownership = ownership;
            return Err(RequestTableError::InvalidTransition);
        };
        stored.record = None;
        Ok(completion)
    }

    pub(super) fn abandon_staged(
        &self,
        token: RequestToken,
    ) -> Result<OwnedRequest, RequestTableError> {
        let mut stored = self.slot(token)?.record.lock();
        let record = checked_record_mut(&mut stored, token)?;
        if record.phase != RequestPhase::Staged {
            return Err(RequestTableError::InvalidTransition);
        }
        let ownership = core::mem::replace(&mut record.ownership, RequestOwnership::Driver);
        let RequestOwnership::Runtime(request) = ownership else {
            record.ownership = ownership;
            return Err(RequestTableError::InvalidTransition);
        };
        stored.record = None;
        Ok(request)
    }

    pub(super) fn earliest_deadline(&self) -> Option<u64> {
        self.slots
            .iter()
            .filter_map(|slot| {
                let stored = slot.record.lock();
                let record = stored.record.as_ref()?;
                (record.phase == RequestPhase::InFlight)
                    .then_some(record.deadline_ns)
                    .flatten()
            })
            .min()
    }

    pub(super) fn has_expired(&self, now_ns: u64) -> bool {
        self.earliest_deadline()
            .is_some_and(|deadline| deadline <= now_ns)
    }

    pub(super) fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Acquire)
    }

    fn decode(&self, id: RequestId) -> Result<RequestToken, RequestTableError> {
        if id.is_inline() {
            return Err(RequestTableError::Stale);
        }
        let encoded = usize::from(id);
        let slot = encoded % self.slots.len();
        let generation = encoded / self.slots.len();
        if generation == 0 {
            return Err(RequestTableError::Stale);
        }
        Ok(RequestToken {
            id,
            slot,
            generation,
        })
    }

    fn slot(&self, token: RequestToken) -> Result<&SharedRequestSlot, RequestTableError> {
        let slot = self.slots.get(token.slot).ok_or(RequestTableError::Stale)?;
        if token.id
            != encode_request_id(token.slot, token.generation, self.slots.len())
                .ok_or(RequestTableError::Stale)?
        {
            return Err(RequestTableError::Stale);
        }
        Ok(slot)
    }

    fn release_inflight(&self) {
        let previous = self.inflight.fetch_sub(1, Ordering::AcqRel);
        assert!(previous != 0, "v0.13 request table inflight underflowed");
    }
}

fn checked_record_mut(
    slot: &mut RequestSlot,
    token: RequestToken,
) -> Result<&mut RequestRecord, RequestTableError> {
    let record = slot.record.as_mut().ok_or(RequestTableError::Stale)?;
    if record.generation != token.generation {
        return Err(RequestTableError::Stale);
    }
    Ok(record)
}

fn completion_ready(slot: &SharedRequestSlot, token: RequestToken) -> bool {
    slot.record.lock().record.as_ref().is_some_and(|record| {
        record.generation == token.generation && record.phase == RequestPhase::Terminal
    })
}

fn encode_request_id(slot: usize, generation: usize, capacity: usize) -> Option<RequestId> {
    let encoded = generation.checked_mul(capacity)?.checked_add(slot)?;
    RequestId::try_new(encoded)
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use rdif_block::{RequestFlags, RequestOp};

    use super::*;

    fn driver_device() -> DriverDeviceKey {
        DriverDeviceKey::new(NonZeroU64::new(1).unwrap())
    }

    fn request() -> OwnedRequest {
        OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        }
    }

    #[test]
    fn driver_observes_only_prepublished_inflight_request() {
        let table = DomainRequestTable::new(1).unwrap();
        let token = table.reserve(7, driver_device(), request()).unwrap();
        let dispatch = table.begin_dispatch(token, 100).unwrap();

        assert_eq!(table.inflight(), 1);
        assert_eq!(table.earliest_deadline(), Some(100));
        assert_eq!(dispatch.id, token.id());
        assert_eq!(dispatch.queue_id, 7);
    }

    #[test]
    fn completion_can_only_claim_inflight_generation() {
        let table = DomainRequestTable::new(1).unwrap();
        let token = table.reserve(3, driver_device(), request()).unwrap();
        let premature = CompletedRequest::new(token.id(), Ok(()), request());

        assert!(matches!(
            table.install_completion(premature),
            Err((RequestTableError::InvalidTransition, _))
        ));
    }

    #[test]
    fn hardware_not_visible_rejection_removes_phantom_inflight() {
        let table = DomainRequestTable::new(1).unwrap();
        let token = table.reserve(3, driver_device(), request()).unwrap();
        let dispatch = table.begin_dispatch(token, 100).unwrap();
        let rejection =
            UnacceptedRequest::new(dispatch.id, BlkError::InvalidRequest, dispatch.request);

        let (error, request) = table
            .restore_unaccepted(dispatch.permit, rejection)
            .unwrap();

        assert_eq!(error, BlkError::InvalidRequest);
        assert_eq!(table.inflight(), 0);
        table.finish_unaccepted(token, error, request).unwrap();
    }
}
