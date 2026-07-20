//! Accepted-request ownership from CPU staging through driver dispatch.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use rdif_block::{BlkError, CompletedRequest, QueueHandle, SubmitOutcome};

use super::{
    DispatchResult, HardwareQueue, HardwareQueueError, RuntimeSubmitError, SubmittedRequest,
};
use crate::block::{HctxCause, RequestTag};

impl SubmittedRequest {
    /// Runtime generation-bearing ID used by watchdog/cancel work.
    pub fn id(&self) -> Result<rdif_block::RequestId, HardwareQueueError> {
        Ok(self.tag.into_request_id()?)
    }

    /// Requests cancellation through the queue's serialized service worker.
    ///
    /// A staged request returns its owned buffer directly from worker context.
    /// An in-flight request first enters controller recovery; its terminal
    /// completion is not published until DMA quiescence returns ownership.
    pub fn request_cancel(&self) -> Result<bool, HardwareQueueError> {
        self.queue.request_cancel(self.tag)
    }

    /// Parks until this generation receives exactly one terminal completion.
    pub fn wait(self) -> Result<CompletedRequest, HardwareQueueError> {
        self.queue
            .requests
            .wait_and_take(self.tag, &self.queue.control)
    }
}

impl HardwareQueue {
    /// Publishes a request before any driver submission can observe its ID.
    pub fn submit_owned(
        self: &Arc<Self>,
        request: rdif_block::OwnedRequest,
    ) -> Result<SubmittedRequest, RuntimeSubmitError> {
        let queue = self.as_ref();
        if ax_hal::irq::in_irq_context() {
            return Err(RuntimeSubmitError::new(
                HardwareQueueError::UnsafeContext,
                request,
            ));
        }
        let _access = match queue.try_driver_access() {
            Some(access) => access,
            None => {
                return Err(RuntimeSubmitError::new(
                    HardwareQueueError::Offline,
                    request,
                ));
            }
        };
        let tag = queue.reserve_submission(request)?;

        // Hardware queues are single-owner objects. A submitting task only
        // publishes request ownership into its software context; the pinned
        // maintenance owner performs every driver call and doorbell write.
        let outcome = self.stage_on_current_cpu(tag);

        match outcome {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let request = queue.requests.abandon(tag).unwrap_or_else(|abandon_error| {
                    panic!(
                        "failed to return unaccepted block request after {error}: {abandon_error}"
                    )
                });
                queue.finish_accepted_request();
                Err(RuntimeSubmitError::new(error, request))
            }
        }
    }

    fn stage_on_current_cpu(
        self: &Arc<Self>,
        tag: RequestTag,
    ) -> Result<SubmittedRequest, HardwareQueueError> {
        let cpu = ax_hal::percpu::this_cpu_id();
        let Some(context) = self.software_contexts.get(cpu) else {
            return Err(HardwareQueueError::InvalidCpu(cpu));
        };
        self.requests.ensure_staged(tag)?;
        context.lock().push(tag)?;
        if let Err(error) = self.queue_service(HctxCause::Submit) {
            let removed = context.lock().remove(tag);
            assert!(removed, "failed work activation lost staged request tag");
            return Err(error);
        }
        Ok(SubmittedRequest {
            queue: Arc::clone(self),
            tag,
        })
    }

    pub(super) fn dispatch_one_locked(
        &self,
        tag: RequestTag,
        driver: &mut QueueHandle,
    ) -> Result<DispatchResult, HardwareQueueError> {
        // Publish the non-claimable dispatch boundary before the driver can
        // observe the request ID. A watchdog never gets to mistake a request
        // that the driver is deciding to keep for software-owned staging.
        let id = tag.into_request_id()?;
        let (permit, request) = self.requests.begin_dispatch(tag)?;
        match driver.submit_owned(id, request) {
            Ok(SubmitOutcome::Queued) => {
                let deadline = ax_hal::time::monotonic_time_nanos()
                    .saturating_add(self.info.limits.request_timeout_ns);
                let recovery_error = permit.commit_queued(deadline).err();
                self.inflight.fetch_add(1, Ordering::AcqRel);
                Ok(DispatchResult::queued(recovery_error))
            }
            Ok(SubmitOutcome::Completed(mut completion)) => {
                permit.commit_inline_return();
                completion.id = id;
                completion.result = Err(BlkError::Io);
                Ok(DispatchResult::terminal(
                    completion,
                    Some(HardwareQueueError::SynchronousCompletion),
                ))
            }
            Err(error) => {
                let (returned_id, driver_error, request) = error.into_parts();
                if let Err(failed) = permit.restore_rejected(request) {
                    let (state_error, request) = failed.into_parts();
                    return Ok(DispatchResult::terminal(
                        CompletedRequest::new(id, Err(BlkError::Io), request),
                        Some(state_error),
                    ));
                }
                if returned_id != id {
                    let request = self.requests.take_staged(tag)?;
                    return Ok(DispatchResult::terminal(
                        CompletedRequest::new(id, Err(BlkError::Io), request),
                        Some(HardwareQueueError::StaleCompletion),
                    ));
                }
                if matches!(driver_error, BlkError::Busy | BlkError::Retry)
                    && self.inflight.load(Ordering::Acquire) != 0
                {
                    Ok(DispatchResult::deferred())
                } else {
                    let request = self.requests.take_staged(tag)?;
                    let recovery_error = matches!(driver_error, BlkError::Busy | BlkError::Retry)
                        .then_some(HardwareQueueError::Driver(driver_error));
                    Ok(DispatchResult::terminal(
                        CompletedRequest::new(id, Err(driver_error), request),
                        recovery_error,
                    ))
                }
            }
        }
    }
}
