//! UVC video stream.

use alloc::{collections::VecDeque, sync::Arc, vec, vec::Vec};
use core::task::{Context, Poll};

use ax_media::videobuffer::{FrameGuard, VbMemOps, VbPoolLease};
use crab_usb::{
    EndpointHandle,
    usb_if::{
        endpoint::{RequestId, TransferRequest},
        err::USBError,
    },
};

use crate::{
    UvcHandle,
    frame::{FrameParser, PushOutcome},
};

pub(crate) const ISO_BATCH: usize = 64;
pub(crate) const ISO_DEPTH: usize = 3;

/// Pending ISO batch.
#[derive(Clone)]
pub struct IsoPending {
    endpoint: EndpointHandle,
    request_id: RequestId,
}

impl IsoPending {
    pub fn new(endpoint: EndpointHandle, request_id: RequestId) -> Self {
        Self {
            endpoint,
            request_id,
        }
    }

    pub(crate) fn poll(&self, cx: &mut Context<'_>) -> Poll<Result<Vec<usize>, USBError>> {
        match self.endpoint.poll_request(self.request_id, cx) {
            Poll::Ready(Ok(completion)) => Poll::Ready(Ok(completion
                .iso_packets
                .iter()
                .map(|packet| packet.actual_length)
                .collect())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(USBError::TransferError(err))),
            Poll::Pending => Poll::Pending,
        }
    }

    pub(crate) fn cancel(&self) -> Result<(), USBError> {
        self.endpoint
            .cancel(self.request_id)
            .map_err(USBError::TransferError)
    }
}

pub(crate) struct FrameAssembler<M: VbMemOps> {
    parser: FrameParser,
    lease: VbPoolLease<M>,
    expected_bytes: Option<usize>,
}

impl<M: VbMemOps> FrameAssembler<M> {
    pub(crate) fn new(
        parser: FrameParser,
        lease: VbPoolLease<M>,
        expected_bytes: Option<usize>,
    ) -> Self {
        Self {
            parser,
            lease,
            expected_bytes,
        }
    }

    pub(crate) fn process_batch(&mut self, data: &[u8], actuals: &[usize], packet_len: usize) {
        for (i, &actual) in actuals.iter().enumerate() {
            if actual < 2 {
                continue;
            }
            let pkt = &data[i * packet_len..i * packet_len + actual];
            self.process_one_packet(pkt);
        }
    }

    fn process_one_packet(&mut self, pkt: &[u8]) {
        let expected = self.expected_bytes;
        let parser = &mut self.parser;
        let lease = &mut self.lease;

        let Some(mut guard) = lease.try_acquire() else {
            return;
        };
        match Self::push_with(parser, &mut guard, pkt) {
            PushOutcome::Pending => {}
            PushOutcome::Completed { bytes } => Self::commit_if_valid(guard, bytes, expected),
            PushOutcome::CompletedAndRetry { bytes } => {
                let valid = expected.is_none_or(|exp| bytes == exp);
                if valid {
                    guard.commit(bytes as u32);
                    let Some(mut next_guard) = lease.try_acquire() else {
                        return;
                    };
                    match Self::push_with(parser, &mut next_guard, pkt) {
                        PushOutcome::Completed { bytes } => {
                            Self::commit_if_valid(next_guard, bytes, expected);
                        }
                        PushOutcome::Pending => {}
                        PushOutcome::CompletedAndRetry { .. } => {
                            debug_assert!(false, "parser must not retry twice");
                        }
                    }
                } else {
                    log::warn!(
                        "[UVC] drop truncated frame: got {} expected {:?}",
                        bytes,
                        expected
                    );
                    match Self::push_with(parser, &mut guard, pkt) {
                        PushOutcome::Pending => {}
                        PushOutcome::Completed { bytes } => {
                            Self::commit_if_valid(guard, bytes, expected);
                        }
                        PushOutcome::CompletedAndRetry { .. } => {
                            unreachable!("parser must not retry twice")
                        }
                    }
                }
            }
        }
    }

    #[inline]
    fn commit_if_valid(guard: FrameGuard<'_, M>, bytes: usize, expected: Option<usize>) {
        if expected.is_none_or(|exp| bytes == exp) {
            guard.commit(bytes as u32);
        } else {
            log::warn!(
                "[UVC] drop truncated frame: got {} expected {:?}",
                bytes,
                expected
            );
        }
    }

    #[inline]
    fn push_with(
        parser: &mut FrameParser,
        guard: &mut FrameGuard<'_, M>,
        pkt: &[u8],
    ) -> PushOutcome {
        let dest = guard.as_mut_slice();
        parser.push_packet(pkt, dest)
    }
}

struct ActiveSlot {
    buffer: Vec<u8>,
    pending: IsoPending,
}

pub(crate) struct IsoStream<H: UvcHandle> {
    handle: Arc<H>,
    endpoint: u8,
    packet_len: usize,
    batch: usize,
    queue: VecDeque<ActiveSlot>,
}

impl<H: UvcHandle> IsoStream<H> {
    pub(crate) fn new(
        handle: Arc<H>,
        endpoint: u8,
        packet_len: usize,
        batch: usize,
        depth: usize,
    ) -> Result<Self, USBError> {
        let packet_lengths = vec![packet_len; batch];
        let mut queue = VecDeque::with_capacity(depth);
        for _ in 0..depth {
            let mut buffer = vec![0u8; packet_len * batch];
            let pending = handle.submit_endpoint_transfer(
                endpoint,
                TransferRequest::iso_in(&mut buffer, &packet_lengths),
            )?;
            queue.push_back(ActiveSlot { buffer, pending });
        }
        Ok(Self {
            handle,
            endpoint,
            packet_len,
            batch,
            queue,
        })
    }

    pub(crate) fn poll_next<M: VbMemOps>(
        &mut self,
        cx: &mut Context<'_>,
        assembler: &mut FrameAssembler<M>,
    ) -> Poll<Result<(), USBError>> {
        let Some(front) = self.queue.front_mut() else {
            return Poll::Pending;
        };
        match front.pending.poll(cx) {
            Poll::Ready(Ok(actuals)) => {
                let mut slot = self.queue.pop_front().unwrap();
                assembler.process_batch(&slot.buffer, &actuals, self.packet_len);
                let packet_lengths = vec![self.packet_len; self.batch];
                match self.handle.submit_endpoint_transfer(
                    self.endpoint,
                    TransferRequest::iso_in(&mut slot.buffer, &packet_lengths),
                ) {
                    Ok(pending) => {
                        slot.pending = pending;
                        self.queue.push_back(slot);
                        Poll::Ready(Ok(()))
                    }
                    Err(err) => Poll::Ready(Err(err)),
                }
            }
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }

    pub(crate) fn cancel_all(&self) {
        for slot in &self.queue {
            let _ = slot.pending.cancel();
        }
    }
}
