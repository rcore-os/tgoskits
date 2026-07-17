//! Generation-bearing request ownership and request-local completion waits.

use core::array;
#[cfg(test)]
use core::mem::ManuallyDrop;

use ax_kspin::SpinNoPreempt;
use rdif_block::{BlkError, CompletedRequest, CompletionSink, OwnedRequest, RequestId};

use super::{CompletionPublicationError, HardwareQueueError, MAX_REQUESTS, RuntimeSubmitError};
use crate::{
    block::{HctxControl, HctxPhase, RequestState, RequestTag, RequestTagSet, TagError},
    task::WaitQueue,
};

struct RequestRecord {
    tag: RequestTag,
    ownership: RequestOwnership,
    deadline_ns: Option<u64>,
}

/// Single source of truth for the owned request backing in one tag slot.
///
/// The atomic [`RequestState`] arbitrates completion, timeout, and cancel
/// claimants. This enum independently guarantees that exactly one context owns
/// the request value while that arbitration runs: runtime staging, the
/// driver/dispatch boundary, a bounded completion batch, or the waiter-visible
/// terminal slot.
enum RequestOwnership {
    Runtime(OwnedRequest),
    Driver,
    Returning,
    Completed(CompletedRequest),
}

struct RequestSlot {
    record: SpinNoPreempt<Option<RequestRecord>>,
    completion_wait: WaitQueue,
}

impl RequestSlot {
    const fn new() -> Self {
        Self {
            record: SpinNoPreempt::new(None),
            completion_wait: WaitQueue::new(),
        }
    }
}

pub(super) struct RequestTable {
    pub(super) tags: RequestTagSet<MAX_REQUESTS>,
    slots: [RequestSlot; MAX_REQUESTS],
}

/// Linear request-table capability for the interval in which the portable
/// driver decides whether it accepted ownership.
///
/// The only legal consumers are [`Self::commit_queued`],
/// [`Self::restore_rejected`], and [`Self::commit_inline_return`]. Keeping the
/// transition capability separate from the owned request prevents callers from
/// updating the tag and request slot through unrelated APIs.
#[must_use = "a dispatch permit must commit queued, rejected, or inline ownership"]
pub(super) struct DispatchPermit<'table> {
    table: &'table RequestTable,
    tag: RequestTag,
}

/// Failed driver rejection rollback that returns the CPU-owned request.
pub(super) struct DispatchRestoreError {
    error: HardwareQueueError,
    request: OwnedRequest,
}

impl DispatchRestoreError {
    pub(super) fn into_parts(self) -> (HardwareQueueError, OwnedRequest) {
        (self.error, self.request)
    }
}

impl DispatchPermit<'_> {
    /// Commits driver ownership and its absolute watchdog deadline.
    pub(super) fn commit_queued(self, deadline_ns: u64) -> Result<(), HardwareQueueError> {
        self.table.commit_dispatch_inflight(self.tag, deadline_ns)
    }

    /// Atomically returns a driver-rejected request to software staging.
    pub(super) fn restore_rejected(
        self,
        request: OwnedRequest,
    ) -> Result<(), DispatchRestoreError> {
        self.table.restore_rejected_dispatch(self.tag, request)
    }

    /// Consumes the dispatch capability while retaining driver ownership for
    /// the caller's immediate completion publication.
    pub(super) fn commit_inline_return(self) {}
}

#[derive(Clone, Copy)]
struct DeadlineEntry {
    tag: RequestTag,
    deadline_ns: u64,
}

impl RequestTable {
    pub(super) fn new() -> Result<Self, TagError> {
        Ok(Self {
            tags: RequestTagSet::new(MAX_REQUESTS)?,
            slots: [const { RequestSlot::new() }; MAX_REQUESTS],
        })
    }

    pub(super) fn reserve(&self, request: OwnedRequest) -> Result<RequestTag, RuntimeSubmitError> {
        let tag = match self.tags.reserve() {
            Ok(tag) => tag,
            Err(error) => return Err(RuntimeSubmitError::new(error.into(), request)),
        };
        let slot = &self.slots[tag.slot()];
        let mut record = slot.record.lock();
        assert!(record.is_none(), "free block request tag retained payload");
        *record = Some(RequestRecord {
            tag,
            ownership: RequestOwnership::Runtime(request),
            deadline_ns: None,
        });
        Ok(tag)
    }

    pub(super) fn begin_dispatch(
        &self,
        tag: RequestTag,
    ) -> Result<(DispatchPermit<'_>, OwnedRequest), HardwareQueueError> {
        let mut slot = self.slots[tag.slot()].record.lock();
        let record = checked_record_mut(&mut slot, tag)?;
        if !matches!(record.ownership, RequestOwnership::Runtime(_)) {
            return Err(HardwareQueueError::StaleCompletion);
        }
        self.tags.begin_dispatch(tag)?;
        let ownership = core::mem::replace(&mut record.ownership, RequestOwnership::Driver);
        let RequestOwnership::Runtime(request) = ownership else {
            unreachable!("validated runtime request ownership changed while its slot was locked");
        };
        Ok((DispatchPermit { table: self, tag }, request))
    }

    fn restore_rejected_dispatch(
        &self,
        tag: RequestTag,
        request: OwnedRequest,
    ) -> Result<(), DispatchRestoreError> {
        let mut slot = self.slots[tag.slot()].record.lock();
        let record = match checked_record_mut(&mut slot, tag) {
            Ok(record) => record,
            Err(error) => return Err(DispatchRestoreError { error, request }),
        };
        restore_rejected_ownership(&self.tags, record, tag, request)
    }

    pub(super) fn take_staged(&self, tag: RequestTag) -> Result<OwnedRequest, HardwareQueueError> {
        let mut slot = self.slots[tag.slot()].record.lock();
        let record = checked_record_mut(&mut slot, tag)?;
        take_runtime_ownership(record)
    }

    fn commit_dispatch_inflight(
        &self,
        tag: RequestTag,
        deadline_ns: u64,
    ) -> Result<(), HardwareQueueError> {
        {
            let mut slot = self.slots[tag.slot()].record.lock();
            let record = checked_record_mut(&mut slot, tag)?;
            if !matches!(record.ownership, RequestOwnership::Driver) {
                return Err(HardwareQueueError::RequestState);
            }
            record.deadline_ns = Some(deadline_ns);
            if let Err(error) = self.tags.mark_inflight(tag) {
                record.deadline_ns = None;
                return Err(error.into());
            }
        }
        Ok(())
    }

    pub(super) fn earliest_deadline(&self) -> Option<u64> {
        earliest_deadline(&self.deadline_snapshot())
    }

    pub(super) fn first_expired(&self, now_ns: u64) -> Option<RequestTag> {
        first_expired(&self.deadline_snapshot(), now_ns)
    }

    pub(super) fn clear_deadline(&self, tag: RequestTag) -> Result<(), HardwareQueueError> {
        let mut slot = self.slots[tag.slot()].record.lock();
        checked_record_mut(&mut slot, tag)?.deadline_ns = None;
        Ok(())
    }

    fn deadline_snapshot(&self) -> [Option<DeadlineEntry>; MAX_REQUESTS] {
        array::from_fn(|slot| {
            let record = self.slots[slot].record.lock();
            let record = record.as_ref()?;
            matches!(record.ownership, RequestOwnership::Driver).then_some(DeadlineEntry {
                tag: record.tag,
                deadline_ns: record.deadline_ns?,
            })
        })
    }

    pub(super) fn ensure_staged(&self, tag: RequestTag) -> Result<(), HardwareQueueError> {
        if self.tags.state(tag)? == RequestState::Reserved {
            self.tags.mark_staged(tag)?;
        }
        Ok(())
    }

    pub(super) fn publish_completion(
        &self,
        tag: RequestTag,
        mut completion: CompletedRequest,
    ) -> Result<bool, CompletionPublicationError> {
        completion.id = match tag.into_request_id() {
            Ok(id) => id,
            Err(error) => {
                return Err(CompletionPublicationError::new(error.into(), completion));
            }
        };
        let slot = &self.slots[tag.slot()];
        let was_inflight = {
            let mut stored = slot.record.lock();
            let record = match checked_record_mut(&mut stored, tag) {
                Ok(record) => record,
                Err(error) => return Err(CompletionPublicationError::new(error, completion)),
            };
            install_completion(&self.tags, record, tag, completion)?
        };
        slot.completion_wait.notify_all();
        Ok(was_inflight)
    }

    fn take_completed(&self, tag: RequestTag) -> Result<CompletedRequest, HardwareQueueError> {
        let slot = &self.slots[tag.slot()];
        let completion = {
            let mut stored = slot.record.lock();
            let record = checked_record_mut(&mut stored, tag)?;
            if !matches!(record.ownership, RequestOwnership::Completed(_)) {
                return Err(HardwareQueueError::StaleCompletion);
            }
            self.tags.release(tag)?;
            let completion = take_completed_ownership(record)?;
            *stored = None;
            completion
        };
        Ok(completion)
    }

    pub(super) fn wait_and_take(
        &self,
        tag: RequestTag,
        control: &HctxControl,
    ) -> Result<CompletedRequest, HardwareQueueError> {
        let slot = &self.slots[tag.slot()];
        slot.completion_wait.try_wait_until(|| {
            completion_is_ready(slot, tag) || control.phase() == HctxPhase::Offline
        })?;
        if completion_is_ready(slot, tag) {
            return self.take_completed(tag);
        }
        match control.phase() {
            HctxPhase::Offline => Err(HardwareQueueError::Offline),
            _ => Err(HardwareQueueError::StaleCompletion),
        }
    }

    pub(super) fn notify_all_waiters_offline(&self) {
        for slot in &self.slots {
            slot.completion_wait.notify_all();
        }
    }

    pub(super) fn timing_out_request_id(&self) -> Option<RequestId> {
        self.slots.iter().find_map(|slot| {
            let record = slot.record.lock();
            let record = record.as_ref()?;
            (self.tags.state(record.tag).ok()? == RequestState::TimingOut)
                .then(|| record.tag.into_request_id().ok())
                .flatten()
        })
    }

    pub(super) fn first_canceling_staged(&self) -> Option<RequestTag> {
        self.slots.iter().find_map(|slot| {
            let record = slot.record.lock();
            let record = record.as_ref()?;
            (matches!(record.ownership, RequestOwnership::Runtime(_))
                && self.tags.state(record.tag).ok()? == RequestState::Canceling)
                .then_some(record.tag)
        })
    }

    pub(super) fn canceling_inflight_request_id(&self) -> Option<RequestId> {
        self.slots.iter().find_map(|slot| {
            let record = slot.record.lock();
            let record = record.as_ref()?;
            (matches!(record.ownership, RequestOwnership::Driver)
                && self.tags.state(record.tag).ok()? == RequestState::Canceling)
                .then(|| record.tag.into_request_id().ok())
                .flatten()
        })
    }

    pub(super) fn complete_canceling_staged(
        &self,
        tag: RequestTag,
    ) -> Result<CompletedRequest, HardwareQueueError> {
        let id = tag.into_request_id()?;
        if self.tags.state(tag)? != RequestState::Canceling {
            return Err(HardwareQueueError::RequestState);
        }
        let mut stored = self.slots[tag.slot()].record.lock();
        let record = checked_record_mut(&mut stored, tag)?;
        let request = take_runtime_ownership(record)?;
        Ok(CompletedRequest::new(id, Err(BlkError::Cancelled), request))
    }

    pub(super) fn reclaim_runtime_owned(
        &self,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), HardwareQueueError> {
        for slot in &self.slots {
            let reclaimed = {
                let mut record = slot.record.lock();
                let Some(record) = record.as_mut() else {
                    continue;
                };
                if !matches!(record.ownership, RequestOwnership::Runtime(_)) {
                    None
                } else {
                    let id = record.tag.into_request_id()?;
                    Some((id, take_runtime_ownership(record)?))
                }
            };
            if let Some((id, request)) = reclaimed {
                sink.complete(CompletedRequest::new(id, Err(BlkError::Cancelled), request));
            }
        }
        Ok(())
    }

    pub(super) fn abandon(&self, tag: RequestTag) -> Result<OwnedRequest, HardwareQueueError> {
        let request = {
            let mut stored = self.slots[tag.slot()].record.lock();
            let record = checked_record_mut(&mut stored, tag)?;
            if !matches!(record.ownership, RequestOwnership::Runtime(_)) {
                return Err(HardwareQueueError::StaleCompletion);
            }
            self.tags.abandon_unaccepted(tag)?;
            let mut record = stored
                .take()
                .expect("validated request record remains present while its slot is locked");
            take_runtime_ownership(&mut record)?
        };
        Ok(request)
    }
}

fn restore_rejected_ownership<const N: usize>(
    tags: &RequestTagSet<N>,
    record: &mut RequestRecord,
    tag: RequestTag,
    request: OwnedRequest,
) -> Result<(), DispatchRestoreError> {
    if !matches!(record.ownership, RequestOwnership::Driver) {
        return Err(DispatchRestoreError {
            error: HardwareQueueError::StaleCompletion,
            request,
        });
    }
    if let Err(error) = tags.restore_after_rejection(tag) {
        return Err(DispatchRestoreError {
            error: error.into(),
            request,
        });
    }
    record.ownership = RequestOwnership::Runtime(request);
    Ok(())
}

fn earliest_deadline(entries: &[Option<DeadlineEntry>]) -> Option<u64> {
    entries
        .iter()
        .flatten()
        .map(|entry| entry.deadline_ns)
        .min()
}

fn first_expired(entries: &[Option<DeadlineEntry>], now_ns: u64) -> Option<RequestTag> {
    entries
        .iter()
        .flatten()
        .find(|entry| entry.deadline_ns <= now_ns)
        .map(|entry| entry.tag)
}

fn checked_record_mut(
    slot: &mut Option<RequestRecord>,
    tag: RequestTag,
) -> Result<&mut RequestRecord, HardwareQueueError> {
    slot.as_mut()
        .filter(|record| record.tag == tag)
        .ok_or(HardwareQueueError::StaleCompletion)
}

fn take_runtime_ownership(record: &mut RequestRecord) -> Result<OwnedRequest, HardwareQueueError> {
    let ownership = core::mem::replace(&mut record.ownership, RequestOwnership::Returning);
    match ownership {
        RequestOwnership::Runtime(request) => Ok(request),
        ownership => {
            record.ownership = ownership;
            Err(HardwareQueueError::StaleCompletion)
        }
    }
}

fn take_completed_ownership(
    record: &mut RequestRecord,
) -> Result<CompletedRequest, HardwareQueueError> {
    let ownership = core::mem::replace(&mut record.ownership, RequestOwnership::Returning);
    match ownership {
        RequestOwnership::Completed(completion) => Ok(completion),
        ownership => {
            record.ownership = ownership;
            Err(HardwareQueueError::StaleCompletion)
        }
    }
}

fn install_completion<const N: usize>(
    tags: &RequestTagSet<N>,
    record: &mut RequestRecord,
    tag: RequestTag,
    mut completion: CompletedRequest,
) -> Result<bool, CompletionPublicationError> {
    let ownership_was_driver = matches!(record.ownership, RequestOwnership::Driver);
    if !matches!(
        record.ownership,
        RequestOwnership::Driver | RequestOwnership::Returning
    ) {
        return Err(CompletionPublicationError::new(
            HardwareQueueError::StaleCompletion,
            completion,
        ));
    }
    let state = match tags.state(tag) {
        Ok(state) => state,
        Err(error) => {
            return Err(CompletionPublicationError::new(error.into(), completion));
        }
    };
    if !ownership_accepts_completion(&record.ownership, state) {
        return Err(CompletionPublicationError::new(
            HardwareQueueError::StaleCompletion,
            completion,
        ));
    }
    let claimed_state = match finish_completion_tag_state(tags, tag, &mut completion) {
        Ok(state) => state,
        Err(error) => return Err(CompletionPublicationError::new(error, completion)),
    };
    let was_inflight = ownership_was_driver && claimed_state != RequestState::Dispatching;
    record.ownership = RequestOwnership::Completed(completion);
    record.deadline_ns = None;
    Ok(was_inflight)
}

fn ownership_accepts_completion(ownership: &RequestOwnership, state: RequestState) -> bool {
    match ownership {
        RequestOwnership::Driver => matches!(
            state,
            RequestState::Dispatching
                | RequestState::InFlight
                | RequestState::TimingOut
                | RequestState::Canceling
        ),
        RequestOwnership::Returning => matches!(
            state,
            RequestState::Staged | RequestState::TimingOut | RequestState::Canceling
        ),
        RequestOwnership::Runtime(_) | RequestOwnership::Completed(_) => false,
    }
}

fn completion_is_ready(slot: &RequestSlot, tag: RequestTag) -> bool {
    slot.record.lock().as_ref().is_some_and(|record| {
        record.tag == tag && matches!(record.ownership, RequestOwnership::Completed(_))
    })
}

fn finish_completion_tag_state<const N: usize>(
    tags: &RequestTagSet<N>,
    tag: RequestTag,
    completion: &mut CompletedRequest,
) -> Result<RequestState, HardwareQueueError> {
    let claimed_state = tags.state(tag)?;
    match claimed_state {
        RequestState::TimingOut => {
            completion.result = Err(BlkError::TimedOut);
            tags.finish_timeout_after_return(tag)?;
        }
        RequestState::Canceling => {
            completion.result = Err(BlkError::Cancelled);
            tags.finish_cancel_after_return(tag)?;
        }
        RequestState::Reserved
        | RequestState::Staged
        | RequestState::Dispatching
        | RequestState::InFlight => {
            tags.claim_completion(tag)?.finish()?;
        }
        _ => return Err(HardwareQueueError::StaleCompletion),
    }
    Ok(claimed_state)
}

#[cfg(test)]
mod tests {
    use rdif_block::{RequestFlags, RequestOp};

    use super::*;

    fn tag(slot: u8, generation: u64) -> RequestTag {
        RequestTag::from_request_id(RequestId::new(generation as usize * 64 + slot as usize))
            .unwrap()
    }

    #[test]
    fn request_deadline_scan_uses_absolute_time_without_device_polling() {
        let later = tag(1, 1);
        let first = tag(2, 1);
        let entries = [
            Some(DeadlineEntry {
                tag: later,
                deadline_ns: 200,
            }),
            Some(DeadlineEntry {
                tag: first,
                deadline_ns: 100,
            }),
        ];

        assert_eq!(earliest_deadline(&entries), Some(100));
        assert_eq!(first_expired(&entries, 99), None);
        assert_eq!(first_expired(&entries, 100), Some(first));
    }

    #[test]
    fn stale_completion_returns_full_ownership_for_quarantine() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        let mut record = RequestRecord {
            tag,
            ownership: RequestOwnership::Completed(CompletedRequest::new(
                tag.into_request_id().unwrap(),
                Ok(()),
                flush_request(),
            )),
            deadline_ns: Some(100),
        };

        let stale = CompletedRequest::new(tag.into_request_id().unwrap(), Ok(()), flush_request());
        let rejected = install_completion(&tags, &mut record, tag, stale)
            .expect_err("a second completion must retain ownership for quarantine");
        let (error, completion) = rejected.into_parts();
        assert!(matches!(error, HardwareQueueError::StaleCompletion));
        let _retained_for_test_shutdown = ManuallyDrop::new(completion);
    }

    #[test]
    fn synchronous_driver_return_completes_the_dispatching_tag_before_recovery() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        tags.mark_staged(tag).unwrap();
        tags.begin_dispatch(tag).unwrap();
        let mut completion = CompletedRequest::new(
            tag.into_request_id().unwrap(),
            Err(BlkError::Io),
            flush_request(),
        );

        finish_completion_tag_state(&tags, tag, &mut completion).unwrap();
        assert_eq!(tags.state(tag), Ok(RequestState::Terminal));
    }

    #[test]
    fn late_irq_completion_cannot_replace_runtime_owned_staging() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        tags.mark_staged(tag).unwrap();
        let mut record = RequestRecord {
            tag,
            ownership: RequestOwnership::Runtime(flush_request()),
            deadline_ns: None,
        };
        let completion =
            CompletedRequest::new(tag.into_request_id().unwrap(), Ok(()), flush_request());

        let rejected = install_completion(&tags, &mut record, tag, completion)
            .expect_err("runtime-owned staging must reject an IRQ completion");
        let (error, completion) = rejected.into_parts();
        assert!(matches!(error, HardwareQueueError::StaleCompletion));
        assert_eq!(tags.state(tag), Ok(RequestState::Staged));
        assert!(
            matches!(record.ownership, RequestOwnership::Runtime(_)),
            "rejected IRQ evidence must leave the runtime-owned request reclaimable"
        );
        drop(completion);
    }

    #[test]
    fn failed_dispatch_restore_returns_request_without_half_committing_slot_ownership() {
        let tags = RequestTagSet::<1>::new(1).unwrap();
        let tag = tags.reserve().unwrap();
        tags.mark_staged(tag).unwrap();
        tags.begin_dispatch(tag).unwrap();
        tags.mark_inflight(tag).unwrap();
        let mut record = RequestRecord {
            tag,
            ownership: RequestOwnership::Driver,
            deadline_ns: None,
        };

        let failed = restore_rejected_ownership(&tags, &mut record, tag, flush_request())
            .expect_err("an in-flight tag cannot be restored as staged");
        let (error, request) = failed.into_parts();

        assert!(matches!(error, HardwareQueueError::RequestState));
        assert_eq!(tags.state(tag), Ok(RequestState::InFlight));
        assert!(matches!(record.ownership, RequestOwnership::Driver));
        drop(request);
    }

    fn flush_request() -> OwnedRequest {
        OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        }
    }
}
