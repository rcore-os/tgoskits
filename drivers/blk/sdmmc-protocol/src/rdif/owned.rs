use log::warn;
use rdif_block::{
    self, BlkError, IQueueOwned, OwnedRequest, PollError, RequestId,
    RequestPoll as OwnedRequestPoll, SubmitError,
};

use crate::{
    BlockPoll, BlockRequestId,
    rdif::{
        config::{block_addr_for_card, device_info, map_dev_err_to_blk_err, queue_limits},
        host::{BlockHost, OwnedBlockSubmitError},
        queue::BlockQueue,
    },
};

impl<H> BlockQueue<H>
where
    H: BlockHost,
{
    fn submit_owned_request_inner(
        &mut self,
        request: OwnedRequest,
    ) -> Result<RequestId, SubmitError> {
        if self.control.config.dma.is_none() {
            return Err(SubmitError::new(BlkError::NotSupported, request));
        }
        if self.split_transfer.is_some() || !self.completed_owned.is_empty() {
            return Err(SubmitError::new(BlkError::Retry, request));
        }
        if let Err(err) = rdif_block::validate_owned_request(self.queue_info(), &request) {
            return Err(SubmitError::new(err, request));
        }
        if let Some(active) = self
            .pending
            .as_ref()
            .map(|pending| RequestId::new(usize::from(H::request_id(pending))))
        {
            match self.poll_owned_request_inner(active) {
                Ok(OwnedRequestPoll::Ready(completed)) => {
                    self.completed_owned.push(completed);
                    return Err(SubmitError::new(BlkError::Retry, request));
                }
                Ok(OwnedRequestPoll::Pending) => {
                    return Err(SubmitError::new(BlkError::Retry, request));
                }
                Err(_) => return Err(SubmitError::new(BlkError::Io, request)),
            }
        }

        let OwnedRequest {
            op,
            lba,
            block_count,
            data,
            flags,
        } = request;
        let Some(buffer) = data else {
            return Err(SubmitError::new(
                BlkError::InvalidRequest,
                OwnedRequest {
                    op,
                    lba,
                    block_count,
                    data: None,
                    flags,
                },
            ));
        };
        let raw = self.control.raw.clone();
        match raw.with_mut(|raw| {
            let start_block = match block_addr_for_card(lba, raw.is_high_capacity()) {
                Ok(start_block) => start_block,
                Err(err) => return Err(OwnedBlockSubmitError::new(err, buffer)),
            };
            match op {
                rdif_block::RequestOp::Read => H::submit_owned_read_request(
                    raw.host_mut(),
                    start_block,
                    buffer,
                    &mut self.slot,
                    &mut self.pending,
                ),
                rdif_block::RequestOp::Write => H::submit_owned_write_request(
                    raw.host_mut(),
                    start_block,
                    buffer,
                    &mut self.slot,
                    &mut self.pending,
                ),
                rdif_block::RequestOp::Flush
                | rdif_block::RequestOp::Discard
                | rdif_block::RequestOp::WriteZeroes => {
                    Err(OwnedBlockSubmitError::new(BlkError::NotSupported, buffer))
                }
            }
        }) {
            Ok(id) => Ok(RequestId::new(usize::from(id))),
            Err(err) => {
                let (error, buffer) = err.into_parts();
                Err(SubmitError::new(
                    error,
                    OwnedRequest {
                        op,
                        lba,
                        block_count,
                        data: Some(buffer),
                        flags,
                    },
                ))
            }
        }
    }

    fn poll_owned_request_inner(
        &mut self,
        request: RequestId,
    ) -> Result<OwnedRequestPoll, PollError> {
        if let Some(index) = self
            .completed_owned
            .iter()
            .position(|completed| completed.id == request)
        {
            return Ok(OwnedRequestPoll::Ready(
                self.completed_owned.swap_remove(index),
            ));
        }
        if self.split_transfer.is_some() {
            return Err(PollError::WrongQueue);
        }
        let id = BlockRequestId::new(usize::from(request));
        let Some(active) = self.pending.as_ref() else {
            return Err(PollError::UnknownRequest);
        };
        if H::request_id(active) != id {
            return Ok(OwnedRequestPoll::Pending);
        }
        let raw = self.control.raw.clone();
        match raw.with_mut(|raw| {
            H::poll_block_request(raw.host_mut(), &mut self.pending, id, &mut self.slot)
        }) {
            Ok(BlockPoll::Pending) => Ok(OwnedRequestPoll::Pending),
            Ok(BlockPoll::Complete) => {
                let completed_dma = H::take_completed_dma(&mut self.slot);
                self.pending = None;
                Ok(OwnedRequestPoll::Ready(rdif_block::CompletedRequest::new(
                    request,
                    Ok(()),
                    completed_dma,
                )))
            }
            Err(err) => {
                let raw = self.control.raw.clone();
                let abort = raw.with_mut(|raw| {
                    H::abort_request(raw.host_mut(), &mut self.pending, &mut self.slot)
                });
                let completed_dma = H::take_completed_dma(&mut self.slot);
                let result = match abort {
                    Ok(()) => Err(map_dev_err_to_blk_err(err)),
                    Err(recovery) => Err(map_dev_err_to_blk_err(recovery)),
                };
                Ok(OwnedRequestPoll::Ready(rdif_block::CompletedRequest::new(
                    request,
                    result,
                    completed_dma,
                )))
            }
        }
    }
}

impl<H> IQueueOwned for BlockQueue<H>
where
    H: BlockHost,
{
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> rdif_block::QueueInfo {
        rdif_block::QueueInfo {
            id: self.id,
            device: device_info(&self.control.config),
            limits: queue_limits(&self.control.config, self.control.config.dma_mask),
        }
    }

    fn submit_request(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
        self.submit_owned_request_inner(request)
    }

    fn poll_request(&mut self, request: RequestId) -> Result<OwnedRequestPoll, PollError> {
        self.poll_owned_request_inner(request)
    }

    fn cancel_request(&mut self, request: RequestId) -> Result<OwnedRequestPoll, PollError> {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| RequestId::new(usize::from(H::request_id(pending))) != request)
        {
            return Err(PollError::UnknownRequest);
        }
        let raw = self.control.raw.clone();
        let result =
            raw.with_mut(|raw| H::abort_request(raw.host_mut(), &mut self.pending, &mut self.slot));
        let completed_dma = H::take_completed_dma(&mut self.slot);
        let completion = match result {
            Ok(()) => rdif_block::CompletedRequest::new(request, Err(BlkError::Io), completed_dma),
            Err(err) => rdif_block::CompletedRequest::new(
                request,
                Err(map_dev_err_to_blk_err(err)),
                completed_dma,
            ),
        };
        Ok(OwnedRequestPoll::Ready(completion))
    }

    fn shutdown(&mut self) {
        if self.pending.is_some() {
            let raw = self.control.raw.clone();
            raw.with_mut(|raw| {
                if let Err(err) =
                    H::abort_request(raw.host_mut(), &mut self.pending, &mut self.slot)
                {
                    warn!(
                        "sdmmc rdif: abort pending owned request on queue shutdown reported \
                         recovery error: {err:?}"
                    );
                    self.pending = None;
                }
            });
        }
    }
}
