use alloc::sync::Arc;

use rdif_block::{BlkError, OwnedRequest, RequestOp, SubmitError, validate_owned_request};

use super::{
    super::{
        channel::SendError,
        completion::{CompletionGroup, CompletionSubscription},
        hctx::request_is_nowait,
    },
    BlockDeviceHandle, DeviceInner, DevicePhase, Submission,
};

enum PermitState {
    Uncommitted,
    Committed,
}

pub(super) struct AdmissionPermit {
    device: Arc<DeviceInner>,
    op: RequestOp,
    count: usize,
    state: PermitState,
}

impl AdmissionPermit {
    fn new(device: Arc<DeviceInner>, op: RequestOp, count: usize) -> Self {
        Self {
            device,
            op,
            count,
            state: PermitState::Uncommitted,
        }
    }

    fn commit_enqueued(mut self) {
        self.state = PermitState::Committed;
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        if matches!(self.state, PermitState::Uncommitted) {
            self.device.undo_submission_admission(self.op, self.count);
        }
    }
}

impl DeviceInner {
    pub(super) async fn acquire_data_async(
        self: &Arc<Self>,
        op: RequestOp,
        count: usize,
        nowait: bool,
    ) -> Result<AdmissionPermit, BlkError> {
        debug_assert!(matches!(op, RequestOp::Read | RequestOp::Write));
        loop {
            let listener = self.listen_for_admission();
            if self.lifecycle_gate.lock().try_admit_data(count)? {
                return Ok(AdmissionPermit::new(Arc::clone(self), op, count));
            }
            if nowait {
                return Err(BlkError::Retry);
            }
            #[cfg(test)]
            self.run_admission_wait_hook();
            listener.await;
        }
    }

    pub(super) async fn acquire_flush_async(
        self: &Arc<Self>,
        nowait: bool,
    ) -> Result<AdmissionPermit, BlkError> {
        let drained = loop {
            let listener = self.listen_for_admission();
            let result = self.lifecycle_gate.lock().try_admit_flush()?;
            match result {
                Some(drained) => break drained,
                None if nowait => return Err(BlkError::Retry),
                None => {
                    #[cfg(test)]
                    self.run_admission_wait_hook();
                    listener.await;
                }
            }
        };

        let permit = AdmissionPermit::new(Arc::clone(self), RequestOp::Flush, 1);
        if drained {
            return Ok(permit);
        }

        loop {
            let listener = self.listen_for_admission();
            let ready = {
                let gate = self.lifecycle_gate.lock();
                if gate.phase != DevicePhase::Ready {
                    return Err(BlkError::Io);
                }
                gate.active_data == 0
            };
            if ready {
                return Ok(permit);
            }
            if nowait {
                return Err(BlkError::Retry);
            }
            #[cfg(test)]
            self.run_admission_wait_hook();
            listener.await;
        }
    }
}

impl BlockDeviceHandle {
    /// Asynchronously admits and enqueues one DMA-owning request.
    ///
    /// The CPU submission channel and queue information are selected and
    /// validated on the first poll. Subsequent polls retain that channel even
    /// if the task resumes on another CPU. This future may only be polled or
    /// dropped from task context or another context where deferred work is
    /// allowed. A request without `NOWAIT` may remain pending even when the
    /// current runtime context cannot block. `NOWAIT` applies only to admission
    /// and enqueue; its completion may still need to wait for hardware.
    ///
    /// Dropping this future before enqueue rolls back its admission. After a
    /// successful enqueue, dropping the returned completion receiver does not
    /// cancel the I/O. Every error returns the original request through
    /// [`SubmitError`].
    pub async fn submit_owned_async(
        &self,
        request: OwnedRequest,
    ) -> Result<CompletionSubscription, SubmitError> {
        if !self
            .inner
            .accepting
            .load(core::sync::atomic::Ordering::Acquire)
        {
            return Err(SubmitError::new(BlkError::Io, request));
        }
        let Some(cpu_channel) = self.inner.select_cpu_channel() else {
            return Err(SubmitError::new(BlkError::Io, request));
        };
        let info = self.inner.effective_queue_info(&cpu_channel);
        if let Err(error) = validate_owned_request(info, &request) {
            return Err(SubmitError::new(error, request));
        }

        let nowait = request_is_nowait(&request);
        let op = request.op;
        let permit = if op == RequestOp::Flush {
            self.inner.acquire_flush_async(nowait).await
        } else {
            self.inner.acquire_data_async(op, 1, nowait).await
        };
        let permit = match permit {
            Ok(permit) => permit,
            Err(error) => return Err(SubmitError::new(error, request)),
        };

        let (group, mut senders) = match CompletionGroup::pairs(1) {
            Ok(pair) => pair,
            Err(error) => return Err(SubmitError::new(error, request)),
        };
        let completion = senders
            .pop_front()
            .expect("single-request completion group has one sender");
        let mut submission = Some(Submission {
            request,
            completion,
        });

        if nowait {
            let item = submission
                .take()
                .expect("NOWAIT submission is attempted exactly once");
            return match cpu_channel.channel.try_enqueue_no_notify_nowait(item) {
                Ok(available) => {
                    permit.commit_enqueued();
                    cpu_channel.channel.notify_enqueued(available);
                    Ok(group
                        .into_single()
                        .expect("single-request submission returns one completion"))
                }
                Err(SendError::Full(item) | SendError::Closed(item)) => Err(SubmitError::new(
                    self.inner.closed_submission_error(),
                    item.request,
                )),
            };
        }

        loop {
            let listener = cpu_channel.channel.listen_for_space();
            let item = submission
                .take()
                .expect("submission remains owned between channel attempts");
            match cpu_channel.channel.try_enqueue_no_notify(item) {
                Ok(available) => {
                    permit.commit_enqueued();
                    cpu_channel.channel.notify_enqueued(available);
                    return Ok(group
                        .into_single()
                        .expect("single-request submission returns one completion"));
                }
                Err(SendError::Full(item)) => {
                    submission = Some(item);
                    #[cfg(test)]
                    cpu_channel.channel.run_space_wait_hook();
                    listener.await;
                }
                Err(SendError::Closed(item)) => {
                    let error = self.inner.closed_submission_error();
                    return Err(SubmitError::new(error, item.request));
                }
            }
        }
    }
}
