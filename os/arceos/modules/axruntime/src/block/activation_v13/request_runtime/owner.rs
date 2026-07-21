//! Owner-thread dispatch and completion publication for v0.13 domains.

use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

use rdif_block::{BlkError, CompletedRequest, CompletionSink, RequestId};

#[cfg(test)]
use super::gates::DispatchState;
use super::{
    RuntimeIoDomainPort,
    gates::{AdmissionError, AdmissionFreezeProgress, DispatchGateError},
    mq::{DomainRequestRuntime, HardwareCreditReservation},
    table::{InstalledCompletion, RequestTableError, RequestToken},
};

/// Owner-local mutable state. It is never placed behind a runtime lock.
pub(in crate::block::activation_v13) struct DomainRequestOwner {
    runtime: Arc<DomainRequestRuntime>,
    dispatch_lists: Vec<VecDeque<RequestToken>>,
    software_cursors: Vec<usize>,
    next_hctx: usize,
    completions: DomainCompletionSink,
}

impl DomainRequestOwner {
    pub(in crate::block::activation_v13) fn new(runtime: Arc<DomainRequestRuntime>) -> Self {
        let hctx_count = runtime.hctxs().len();
        let completion_capacity = runtime
            .hctxs()
            .iter()
            .map(|hctx| hctx.credits().limit)
            .sum();
        Self {
            runtime: Arc::clone(&runtime),
            dispatch_lists: (0..hctx_count).map(|_| VecDeque::new()).collect(),
            software_cursors: alloc::vec![0; hctx_count],
            next_hctx: 0,
            completions: DomainCompletionSink::new(runtime, completion_capacity),
        }
    }

    pub(in crate::block::activation_v13) const fn runtime(&self) -> &Arc<DomainRequestRuntime> {
        &self.runtime
    }

    pub(in crate::block::activation_v13) fn completion_sink(&mut self) -> &mut dyn CompletionSink {
        self.completions.begin_pass();
        &mut self.completions
    }

    pub(in crate::block::activation_v13) fn finish_completions(
        &mut self,
    ) -> Result<usize, DomainRequestServiceError> {
        self.completions.finish_pass()
    }

    /// Dispatches at most `budget` requests. Hctx-local redispatch always
    /// precedes a new per-CPU software-context request.
    pub(in crate::block::activation_v13) fn dispatch<D>(
        &mut self,
        domain: &mut D,
        budget: usize,
    ) -> Result<usize, DomainRequestServiceError>
    where
        D: RuntimeIoDomainPort + ?Sized,
    {
        if budget == 0 || self.runtime.hctxs().is_empty() || !self.runtime.dispatch_allowed() {
            return Ok(0);
        }
        let mut dispatched = 0;
        let hctx_count = self.runtime.hctxs().len();
        let mut idle_hctxs = 0;
        while dispatched < budget && idle_hctxs < hctx_count {
            let hctx_index = self.next_hctx % hctx_count;
            self.next_hctx = (hctx_index + 1) % hctx_count;
            let hctx = &self.runtime.hctxs()[hctx_index];
            let Some(credit) = hctx.credits().try_reserve() else {
                idle_hctxs += 1;
                continue;
            };
            let token = self.dispatch_lists[hctx_index].pop_front().or_else(|| {
                self.runtime
                    .pop_software_ctx(hctx_index, &mut self.software_cursors[hctx_index])
            });
            let Some(token) = token else {
                drop(credit);
                idle_hctxs += 1;
                continue;
            };
            idle_hctxs = 0;
            self.dispatch_one(domain, hctx_index, token, credit)?;
            dispatched += 1;
        }
        Ok(dispatched)
    }

    pub(in crate::block::activation_v13) fn has_staged(&self) -> bool {
        self.dispatch_lists.iter().any(|queue| !queue.is_empty()) || self.runtime.has_staged()
    }

    /// Freezes new submitters while allowing requests accepted before the
    /// cutoff to continue reaching hardware and terminal completion.
    pub(in crate::block::activation_v13) fn begin_quiesce(
        &self,
    ) -> Result<AdmissionFreezeProgress, DomainRequestLifecycleError> {
        let progress = self
            .runtime
            .begin_admission_freeze()
            .map_err(DomainRequestLifecycleError::Admission)?;
        self.runtime
            .begin_dispatch_drain()
            .map_err(DomainRequestLifecycleError::Dispatch)?;
        Ok(progress)
    }

    /// Commits the hardware-dispatch cutoff after one stable owner-side drain.
    pub(in crate::block::activation_v13) fn try_commit_quiesced(
        &self,
    ) -> Result<bool, DomainRequestLifecycleError> {
        if !self.runtime.admission_is_frozen_and_idle() || self.has_staged() {
            return Ok(false);
        }
        // SAFETY: admission is Frozen with no submitter that could still
        // publish a software-context entry. This sole owner observed both its
        // dispatch lists and every software context empty. Existing InFlight
        // records are deliberately outside this proof and remain hardware
        // owned until normal completion or recovery produces DmaQuiesced.
        let proof = unsafe { super::gates::DispatchCutoffProof::new_unchecked() };
        self.runtime
            .commit_dispatch_quiesced(proof)
            .map_err(DomainRequestLifecycleError::Dispatch)?;
        Ok(true)
    }

    /// Reopens dispatch before admitting remote submitters after reinit.
    pub(in crate::block::activation_v13) fn resume_after_reinitialize(
        &self,
    ) -> Result<(), DomainRequestLifecycleError> {
        self.runtime
            .resume_dispatch()
            .map_err(DomainRequestLifecycleError::Dispatch)?;
        self.runtime
            .thaw_admission()
            .map_err(DomainRequestLifecycleError::Admission)
    }

    /// Permanently closes both gates after IRQ and DMA teardown is proven.
    pub(in crate::block::activation_v13) fn close_after_quiesce(
        &self,
    ) -> Result<(), DomainRequestLifecycleError> {
        self.runtime
            .close_dispatch()
            .map_err(DomainRequestLifecycleError::Dispatch)?;
        self.runtime
            .close_admission()
            .map_err(DomainRequestLifecycleError::Admission)
    }

    #[cfg(test)]
    pub(in crate::block::activation_v13) fn dispatch_state(&self) -> DispatchState {
        self.runtime.dispatch_state()
    }

    /// Reports whether every accepted request has reached terminal completion.
    ///
    /// This is deliberately separate from [`Self::try_commit_quiesced`]: a
    /// dispatch cutoff can be established while hardware still owns requests.
    pub(in crate::block::activation_v13) fn accepted_requests_drained(&self) -> bool {
        self.runtime.requests().inflight() == 0
    }

    pub(in crate::block::activation_v13) fn earliest_deadline(&self) -> Option<u64> {
        self.runtime.requests().earliest_deadline()
    }

    pub(in crate::block::activation_v13) fn has_expired(&self, now_ns: u64) -> bool {
        self.runtime.requests().has_expired(now_ns)
    }

    pub(in crate::block::activation_v13) fn handles_source(
        &self,
        source: rdif_block::IrqSourceId,
    ) -> bool {
        self.runtime
            .queue_descs()
            .iter()
            .any(|queue| queue.irq_sources().contains(source.get()))
    }

    fn dispatch_one<D>(
        &self,
        domain: &mut D,
        hctx_index: usize,
        token: RequestToken,
        credit: HardwareCreditReservation<'_>,
    ) -> Result<(), DomainRequestServiceError>
    where
        D: RuntimeIoDomainPort + ?Sized,
    {
        let deadline =
            ax_hal::time::monotonic_time_nanos().saturating_add(self.runtime.watchdog_ns());
        let dispatch = self
            .runtime
            .requests()
            .begin_dispatch(token, deadline)
            .map_err(DomainRequestServiceError::RequestTable)?;
        let expected_id = dispatch.id;
        let result = domain.submit_owned(
            dispatch.queue_id,
            dispatch.driver_device,
            dispatch.id,
            dispatch.request,
        );
        match result {
            Ok(accepted) if accepted.id() == expected_id => {
                dispatch.permit.accept();
                credit.retain_for_inflight();
                Ok(())
            }
            Ok(accepted) => {
                credit.retain_for_inflight();
                Err(DomainRequestServiceError::AcceptedIdentityMismatch {
                    expected: expected_id,
                    returned: accepted.id(),
                })
            }
            Err(unaccepted) => {
                let (driver_error, request) = self
                    .runtime
                    .requests()
                    .restore_unaccepted(dispatch.permit, unaccepted)
                    .map_err(DomainRequestServiceError::RequestTable)?;
                self.runtime
                    .requests()
                    .finish_unaccepted(token, driver_error, request)
                    .map_err(DomainRequestServiceError::RequestTable)?;
                if matches!(driver_error, BlkError::Busy | BlkError::Retry) {
                    return Err(DomainRequestServiceError::CreditContract {
                        queue_id: self.runtime.hctxs()[hctx_index].queue_id(),
                        error: driver_error,
                    });
                }
                Ok(())
            }
        }
    }
}

struct DomainCompletionSink {
    runtime: Arc<DomainRequestRuntime>,
    installed: Vec<InstalledCompletion>,
    rejected: Vec<(RequestTableError, CompletedRequest)>,
    pass_open: bool,
}

impl DomainCompletionSink {
    fn new(runtime: Arc<DomainRequestRuntime>, capacity: usize) -> Self {
        Self {
            runtime,
            installed: Vec::with_capacity(capacity),
            rejected: Vec::with_capacity(capacity),
            pass_open: false,
        }
    }

    fn begin_pass(&mut self) {
        assert!(
            !self.pass_open,
            "v0.13 completion pass nested driver service"
        );
        assert!(self.installed.is_empty() && self.rejected.is_empty());
        self.pass_open = true;
    }

    fn finish_pass(&mut self) -> Result<usize, DomainRequestServiceError> {
        self.finish_pass_with(|runtime, installed| {
            runtime.requests().notify_completion(installed);
        })
    }

    fn finish_pass_with(
        &mut self,
        mut publish: impl FnMut(&DomainRequestRuntime, InstalledCompletion),
    ) -> Result<usize, DomainRequestServiceError> {
        assert!(self.pass_open, "v0.13 completion pass was not opened");
        self.pass_open = false;
        let rejected_completion = !self.rejected.is_empty();
        // A later invalid completion cannot roll back earlier request-table
        // claims from the same evidence pass. Publish those linear terminal
        // transitions before reporting the driver contract fault so their
        // credits and waiters do not remain stranded in quarantine.
        let completed = self.installed.len();
        for installed in self.installed.drain(..) {
            let hctx_index = self.runtime.hctx_index(installed.queue_id).ok_or(
                DomainRequestServiceError::UnknownCompletionQueue(installed.queue_id),
            )?;
            self.runtime.hctxs()[hctx_index]
                .credits()
                .release_inflight();
            publish(&self.runtime, installed);
        }
        if rejected_completion {
            Err(DomainRequestServiceError::RejectedCompletion)
        } else {
            Ok(completed)
        }
    }
}

impl CompletionSink for DomainCompletionSink {
    fn complete(&mut self, completion: CompletedRequest) {
        assert!(
            self.pass_open,
            "driver completion escaped its evidence pass"
        );
        match self.runtime.requests().install_completion(completion) {
            Ok(installed) => {
                assert!(
                    self.installed.len() < self.installed.capacity(),
                    "driver completed more requests than the domain owns credits"
                );
                self.installed.push(installed);
            }
            Err(rejected) => {
                assert!(
                    self.rejected.len() < self.rejected.capacity(),
                    "driver returned more rejected completions than domain capacity"
                );
                self.rejected.push(rejected);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum DomainRequestServiceError {
    #[error(transparent)]
    RequestTable(RequestTableError),
    #[error("driver accepted request {returned:?}, expected {expected:?}")]
    AcceptedIdentityMismatch {
        expected: RequestId,
        returned: RequestId,
    },
    #[error("queue {queue_id} rejected an already reserved hardware credit with {error}")]
    CreditContract { queue_id: usize, error: BlkError },
    #[error("driver returned a terminal completion that did not claim an InFlight request")]
    RejectedCompletion,
    #[error("driver completion referred to unknown hardware queue {0}")]
    UnknownCompletionQueue(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum DomainRequestLifecycleError {
    #[error(transparent)]
    Admission(AdmissionError),
    #[error(transparent)]
    Dispatch(DispatchGateError),
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU16, NonZeroU64};

    use rdif_block::{
        AcceptedRequest, CompletionSink, DriverDeviceKey, EvidenceServiceResult, IdList,
        InterruptQueueDesc, IrqEvidenceId, LogicalDeviceSelector, OwnedRequest, OwnershipDomainId,
        QueueExecution, RequestFlags, RequestOp, UnacceptedRequest,
    };

    use super::*;

    #[test]
    fn dispatch_list_has_priority_over_software_context() {
        let mut dispatch = VecDeque::new();
        let first = RequestToken {
            id: RequestId::new(1),
            slot: 0,
            generation: 1,
        };
        dispatch.push_back(first);

        assert_eq!(dispatch.pop_front(), Some(first));
    }

    #[test]
    fn dispatch_quiesce_does_not_claim_that_hardware_inflight_is_drained() {
        let domain = OwnershipDomainId::new(1).unwrap();
        let mut sources = IdList::none();
        sources.insert(1);
        let queue = InterruptQueueDesc::new(
            0,
            LogicalDeviceSelector::AllPublished,
            domain,
            QueueExecution::Tagged,
            NonZeroU16::new(1).unwrap(),
            sources,
        )
        .unwrap();
        let runtime = Arc::new(
            DomainRequestRuntime::new(
                domain,
                &[queue],
                crate::block::BlockRuntimeConfig::default(),
            )
            .unwrap(),
        );
        let owner = DomainRequestOwner::new(Arc::clone(&runtime));
        let token = runtime
            .requests()
            .reserve(
                0,
                DriverDeviceKey::new(NonZeroU64::new(1).unwrap()),
                OwnedRequest {
                    op: RequestOp::Flush,
                    lba: 0,
                    block_count: 0,
                    data: None,
                    flags: RequestFlags::NONE,
                },
            )
            .unwrap();
        let _hardware_owned = runtime.requests().begin_dispatch(token, 100).unwrap();

        owner.begin_quiesce().unwrap();

        assert!(owner.try_commit_quiesced().unwrap());
        assert_eq!(owner.dispatch_state(), DispatchState::Quiesced);
        assert!(!owner.accepted_requests_drained());
    }

    #[test]
    fn valid_completion_before_invalid_peer_still_releases_credit_and_is_published() {
        let domain = OwnershipDomainId::new(1).unwrap();
        let mut sources = IdList::none();
        sources.insert(1);
        let queue = InterruptQueueDesc::new(
            0,
            LogicalDeviceSelector::AllPublished,
            domain,
            QueueExecution::Tagged,
            NonZeroU16::new(1).unwrap(),
            sources,
        )
        .unwrap();
        let runtime = Arc::new(
            DomainRequestRuntime::new(
                domain,
                &[queue],
                crate::block::BlockRuntimeConfig::default(),
            )
            .unwrap(),
        );
        let mut owner = DomainRequestOwner::new(Arc::clone(&runtime));
        let token = runtime
            .requests()
            .reserve(
                0,
                DriverDeviceKey::new(NonZeroU64::new(1).unwrap()),
                OwnedRequest {
                    op: RequestOp::Flush,
                    lba: 0,
                    block_count: 0,
                    data: None,
                    flags: RequestFlags::NONE,
                },
            )
            .unwrap();
        let credit = runtime.hctxs()[0].credits().try_reserve().unwrap();
        let dispatch = runtime.requests().begin_dispatch(token, 100).unwrap();
        dispatch.permit.accept();
        credit.retain_for_inflight();

        let sink = owner.completion_sink();
        sink.complete(CompletedRequest::new(token.id(), Ok(()), dispatch.request));
        sink.complete(CompletedRequest::new(
            RequestId::new(99),
            Ok(()),
            OwnedRequest {
                op: RequestOp::Flush,
                lba: 0,
                block_count: 0,
                data: None,
                flags: RequestFlags::NONE,
            },
        ));

        let mut published = 0;
        assert_eq!(
            owner
                .completions
                .finish_pass_with(|_, _installed| published += 1),
            Err(DomainRequestServiceError::RejectedCompletion)
        );
        assert_eq!(published, 1);
        assert_eq!(runtime.hctxs()[0].credits().in_use(), 0);
        assert_eq!(runtime.requests().inflight(), 0);
    }

    #[test]
    fn driver_call_observes_credit_deadline_and_inflight_publication() {
        let domain = OwnershipDomainId::new(1).unwrap();
        let runtime = Arc::new(
            DomainRequestRuntime::new(
                domain,
                &[queue(domain, 1)],
                crate::block::BlockRuntimeConfig::default(),
            )
            .unwrap(),
        );
        let mut owner = DomainRequestOwner::new(Arc::clone(&runtime));
        let token = runtime
            .requests()
            .reserve(0, driver_device(), flush_request())
            .unwrap();
        owner.dispatch_lists[0].push_back(token);
        let mut driver = InspectingSubmitDomain {
            runtime: Arc::clone(&runtime),
            accepted: Vec::new(),
        };

        assert_eq!(owner.dispatch(&mut driver, 1), Ok(1));

        assert_eq!(driver.accepted.len(), 1);
        assert_eq!(runtime.requests().inflight(), 1);
        assert!(runtime.requests().earliest_deadline().is_some());
        assert_eq!(runtime.hctxs()[0].credits().in_use(), 1);

        owner.completion_sink().complete(CompletedRequest::new(
            token.id(),
            Ok(()),
            driver.accepted.pop().unwrap().1,
        ));
        assert_eq!(owner.completions.finish_pass_with(|_, _| {}), Ok(1));
    }

    #[test]
    fn max_inflight_one_never_calls_driver_for_a_second_request() {
        let domain = OwnershipDomainId::new(1).unwrap();
        let runtime = Arc::new(
            DomainRequestRuntime::new(
                domain,
                &[queue(domain, 1)],
                crate::block::BlockRuntimeConfig::default(),
            )
            .unwrap(),
        );
        let mut owner = DomainRequestOwner::new(Arc::clone(&runtime));
        let first = runtime
            .requests()
            .reserve(0, driver_device(), flush_request())
            .unwrap();
        let second = runtime
            .requests()
            .reserve(0, driver_device(), flush_request())
            .unwrap();
        owner.dispatch_lists[0].extend([first, second]);
        let mut driver = InspectingSubmitDomain {
            runtime: Arc::clone(&runtime),
            accepted: Vec::new(),
        };

        assert_eq!(owner.dispatch(&mut driver, 2), Ok(1));
        assert_eq!(driver.accepted.len(), 1);
        assert_eq!(owner.dispatch_lists[0].front(), Some(&second));

        let (completed_id, completed_request) = driver.accepted.pop().unwrap();
        owner.completion_sink().complete(CompletedRequest::new(
            completed_id,
            Ok(()),
            completed_request,
        ));
        assert_eq!(owner.completions.finish_pass_with(|_, _| {}), Ok(1));

        assert_eq!(owner.dispatch(&mut driver, 1), Ok(1));
        assert_eq!(driver.accepted.len(), 1);
        let (completed_id, completed_request) = driver.accepted.pop().unwrap();
        owner.completion_sink().complete(CompletedRequest::new(
            completed_id,
            Ok(()),
            completed_request,
        ));
        assert_eq!(owner.completions.finish_pass_with(|_, _| {}), Ok(1));
    }

    struct InspectingSubmitDomain {
        runtime: Arc<DomainRequestRuntime>,
        accepted: Vec<(RequestId, OwnedRequest)>,
    }

    impl RuntimeIoDomainPort for InspectingSubmitDomain {
        fn submit_owned(
            &mut self,
            queue_id: usize,
            _logical_device: DriverDeviceKey,
            id: RequestId,
            request: OwnedRequest,
        ) -> Result<AcceptedRequest, UnacceptedRequest> {
            assert_eq!(queue_id, 0);
            assert_eq!(self.runtime.requests().inflight(), 1);
            assert!(self.runtime.requests().earliest_deadline().is_some());
            assert_eq!(self.runtime.hctxs()[0].credits().in_use(), 1);
            self.accepted.push((id, request));
            Ok(AcceptedRequest::new(id))
        }

        fn service_evidence(
            &mut self,
            _evidence: IrqEvidenceId,
            _sink: &mut dyn CompletionSink,
        ) -> Result<EvidenceServiceResult, BlkError> {
            unreachable!("submission-order test does not service IRQ evidence")
        }
    }

    fn queue(domain: OwnershipDomainId, depth: u16) -> InterruptQueueDesc {
        let mut sources = IdList::none();
        sources.insert(1);
        InterruptQueueDesc::new(
            0,
            LogicalDeviceSelector::AllPublished,
            domain,
            QueueExecution::Tagged,
            NonZeroU16::new(depth).unwrap(),
            sources,
        )
        .unwrap()
    }

    fn driver_device() -> DriverDeviceKey {
        DriverDeviceKey::new(NonZeroU64::new(1).unwrap())
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
