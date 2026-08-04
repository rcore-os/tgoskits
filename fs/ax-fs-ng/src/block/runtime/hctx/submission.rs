use super::*;

struct SubmissionMetadata {
    completion: CompletionSender,
    op: RequestOp,
    block_count: u32,
}

struct AcceptedRequestIds {
    ids: Vec<RequestId>,
}

impl SubmissionSink for AcceptedRequestIds {
    fn accepted(&mut self, id: RequestId) {
        self.ids.push(id);
    }
}

pub(super) struct SubmissionScratch {
    submissions: VecDeque<Submission>,
    requests: OwnedRequestBatch,
    metadata: VecDeque<SubmissionMetadata>,
    accepted: AcceptedRequestIds,
}

impl SubmissionScratch {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            submissions: VecDeque::with_capacity(capacity),
            requests: OwnedRequestBatch::with_capacity(capacity),
            metadata: VecDeque::with_capacity(capacity),
            accepted: AcceptedRequestIds {
                ids: Vec::with_capacity(capacity),
            },
        }
    }
}

pub(super) struct SubmissionLoop<'a> {
    pub(super) state: &'a HctxState,
    pub(super) pending: &'a mut BTreeMap<RequestId, PendingRequest>,
    pub(super) retry_submissions: &'a mut VecDeque<Submission>,
    pub(super) protocol_failed: &'a mut Vec<PendingRequest>,
    pub(super) fatal_error: &'a mut Option<BlkError>,
    pub(super) next_channel: &'a mut usize,
    pub(super) prefer_retry: &'a mut bool,
    pub(super) scratch: &'a mut SubmissionScratch,
}

#[derive(Default)]
pub(super) struct SubmissionProgress {
    pub(super) made_progress: bool,
    pub(super) queue_full: bool,
}

struct SubmissionReconciliation<'a> {
    deadline: Duration,
    pending: &'a mut BTreeMap<RequestId, PendingRequest>,
    retry_submissions: &'a mut VecDeque<Submission>,
    protocol_failed: &'a mut Vec<PendingRequest>,
}

pub(super) fn submit_available(
    queue: &mut dyn HardwareQueue,
    context: SubmissionLoop<'_>,
) -> SubmissionProgress {
    let mut progress = SubmissionProgress::default();
    let limits = queue.info().limits;
    while context.pending.len() < limits.max_inflight {
        let available = limits.max_inflight - context.pending.len();
        let batch_limit = available.min(limits.max_submit_batch);
        collect_submission_batch(
            context.state,
            context.retry_submissions,
            context.next_channel,
            context.prefer_retry,
            batch_limit,
            &mut context.scratch.submissions,
        );
        if context.scratch.submissions.is_empty() {
            break;
        }

        let offered = context.scratch.submissions.len();
        split_submission_batch(
            &mut context.scratch.submissions,
            &mut context.scratch.requests,
            &mut context.scratch.metadata,
        );
        context.scratch.accepted.ids.clear();
        let result =
            queue.submit_batch_owned(&mut context.scratch.requests, &mut context.scratch.accepted);
        let remaining_count_valid = context.scratch.requests.len() <= offered;
        let removed = if remaining_count_valid {
            offered - context.scratch.requests.len()
        } else {
            0
        };
        let contract_valid = context.scratch.requests.len() <= offered
            && result.accepted() == removed
            && context.scratch.accepted.ids.len() == removed;

        if removed != 0 {
            let commit_result = queue.commit_submissions();
            super::super::metrics::record_commit(commit_result);
            if commit_result.is_err() {
                set_hctx_fatal(context.state, context.fatal_error, BlkError::Io);
            }
        }

        let deadline = wall_time().saturating_add(REQUEST_TIMEOUT);
        let ownership_valid = reconcile_submission_batch(
            &mut context.scratch.requests,
            &mut context.scratch.metadata,
            &context.scratch.accepted.ids,
            removed,
            SubmissionReconciliation {
                deadline,
                pending: context.pending,
                retry_submissions: context.retry_submissions,
                protocol_failed: context.protocol_failed,
            },
        );
        super::super::metrics::record_submission_batch(removed, context.pending.len());

        progress.made_progress |= removed != 0;
        if !contract_valid || !ownership_valid {
            set_hctx_fatal(context.state, context.fatal_error, BlkError::Io);
        }
        match result.disposition() {
            BatchSubmitDisposition::Continue => {
                if removed == 0 && !context.retry_submissions.is_empty() {
                    set_hctx_fatal(context.state, context.fatal_error, BlkError::Io);
                }
            }
            BatchSubmitDisposition::QueueFull => {
                progress.queue_full = true;
                break;
            }
            BatchSubmitDisposition::Fatal(error) => {
                set_hctx_fatal(context.state, context.fatal_error, error);
            }
        }
        if context.state.stopping.load(Ordering::Acquire) {
            break;
        }
    }
    progress
}

pub(super) fn collect_submission_batch(
    state: &HctxState,
    retry_submissions: &mut VecDeque<Submission>,
    next_channel: &mut usize,
    prefer_retry: &mut bool,
    limit: usize,
    submissions: &mut VecDeque<Submission>,
) {
    debug_assert!(submissions.is_empty());
    if retry_submissions.is_empty() {
        if drain_submission_channels(state, next_channel, limit, submissions) != 0 {
            *prefer_retry = true;
        }
        return;
    }

    while submissions.len() < limit {
        let submission = if *prefer_retry {
            retry_submissions
                .pop_front()
                .or_else(|| try_recv_submission(state, next_channel))
        } else {
            try_recv_submission(state, next_channel).or_else(|| retry_submissions.pop_front())
        };
        let Some(submission) = submission else {
            break;
        };
        *prefer_retry = !*prefer_retry;
        submissions.push_back(submission);
    }
}

fn drain_submission_channels(
    state: &HctxState,
    next_channel: &mut usize,
    limit: usize,
    submissions: &mut VecDeque<Submission>,
) -> usize {
    if limit == 0 {
        return 0;
    }
    let channels = state.submission_channels.lock();
    if channels.is_empty() {
        return 0;
    }

    let quantum = limit.div_ceil(channels.len()).max(1);
    let mut received = 0;
    let mut empty_channels = 0;
    while received < limit && empty_channels < channels.len() {
        let index = *next_channel % channels.len();
        let count = channels[index].try_recv_many(submissions, quantum.min(limit - received));
        *next_channel = (index + 1) % channels.len();
        received += count;
        if count == 0 {
            empty_channels += 1;
        } else {
            empty_channels = 0;
        }
    }
    received
}

fn split_submission_batch(
    submissions: &mut VecDeque<Submission>,
    requests: &mut OwnedRequestBatch,
    metadata: &mut VecDeque<SubmissionMetadata>,
) {
    debug_assert!(requests.is_empty());
    debug_assert!(metadata.is_empty());
    while let Some(submission) = submissions.pop_front() {
        let Submission {
            mut request,
            completion,
        } = submission;
        request.flags = request.flags.without(RequestFlags::NOWAIT);
        let op = request.op;
        let block_count = request.block_count;
        requests.push_back(request);
        metadata.push_back(SubmissionMetadata {
            completion,
            op,
            block_count,
        });
    }
}

fn reconcile_submission_batch(
    requests: &mut OwnedRequestBatch,
    metadata: &mut VecDeque<SubmissionMetadata>,
    accepted_ids: &[RequestId],
    removed: usize,
    context: SubmissionReconciliation<'_>,
) -> bool {
    let mut ownership_valid = accepted_ids.len() == removed && removed <= metadata.len();
    for index in 0..removed.min(metadata.len()) {
        let request = pending_from_metadata(
            metadata
                .pop_front()
                .expect("removed request metadata count was checked"),
            context.deadline,
        );
        let Some(id) = accepted_ids.get(index).copied() else {
            context.protocol_failed.push(request);
            ownership_valid = false;
            continue;
        };
        match context.pending.entry(id) {
            alloc::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(request);
            }
            alloc::collections::btree_map::Entry::Occupied(_) => {
                context.protocol_failed.push(request);
                ownership_valid = false;
            }
        }
    }

    let request_count = requests.len();
    let metadata_count = metadata.len();
    ownership_valid &= request_count == metadata_count;
    let paired_count = request_count.min(metadata_count);
    for _ in 0..paired_count {
        let request = requests
            .pop_back()
            .expect("runtime-owned request pair count was checked");
        let metadata = metadata
            .pop_back()
            .expect("runtime-owned metadata pair count was checked");
        context.retry_submissions.push_front(Submission {
            request,
            completion: metadata.completion,
        });
    }
    while let Some(request) = requests.pop_front() {
        drop(super::super::dma::complete_without_submit(request.data));
    }
    while let Some(request) = metadata.pop_front() {
        context
            .protocol_failed
            .push(pending_from_metadata(request, context.deadline));
    }
    ownership_valid
}

fn pending_from_metadata(metadata: SubmissionMetadata, deadline: Duration) -> PendingRequest {
    PendingRequest {
        completion: metadata.completion,
        op: metadata.op,
        block_count: metadata.block_count,
        deadline,
    }
}

fn try_recv_submission(state: &HctxState, next_channel: &mut usize) -> Option<Submission> {
    let channels = state.submission_channels.lock();
    if channels.is_empty() {
        return None;
    }
    for offset in 0..channels.len() {
        let index = (*next_channel + offset) % channels.len();
        if let Some(submission) = channels[index].try_recv() {
            *next_channel = (index + 1) % channels.len();
            return Some(submission);
        }
    }
    None
}

pub(super) fn reject_unsubmitted(submission: Submission, observer: &Weak<dyn HctxObserver>) {
    let op = submission.request.op;
    let block_count = submission.request.block_count;
    let data = super::super::dma::complete_without_submit(submission.request.data);
    submission.completion.complete(CompletedRequest::new(
        RequestId::new(usize::MAX),
        Err(BlkError::Io),
        data,
    ));
    notify_observer(observer, op, block_count, Err(BlkError::Io));
}
