use alloc::{boxed::Box, vec::Vec};
use core::any::Any;
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use usb_if::{
    descriptor::EndpointType,
    endpoint::{
        EndpointInfo, IsoPacketResult, RequestId, TransferCompletion, TransferRequest,
        TransferStatus,
    },
    err::TransferError,
};

use super::transfer::Transfer;

mod ctrl;

pub(crate) trait EndpointOp: Send + Any + 'static {
    fn submit_request(&mut self, request: TransferRequest) -> Result<RequestId, TransferError>;

    fn reclaim_request(
        &mut self,
        id: RequestId,
    ) -> Option<Result<TransferCompletion, TransferError>>;

    fn register_waker(&self, id: RequestId, cx: &mut Context<'_>);

    fn cancel_request(&mut self, _id: RequestId) -> Result<(), TransferError> {
        Err(TransferError::NotSupported)
    }
}

pub struct Endpoint {
    info: EndpointInfo,
    raw: Box<dyn EndpointOp>,
}

impl Endpoint {
    pub(crate) fn new(info: EndpointInfo, raw: impl EndpointOp) -> Self {
        Self {
            info,
            raw: Box::new(raw),
        }
    }

    pub fn info(&self) -> EndpointInfo {
        self.info
    }

    pub fn submit(&mut self, request: TransferRequest) -> Result<RequestId, TransferError> {
        self.validate_request(&request)?;
        self.raw.submit_request(request)
    }

    pub fn reclaim(&mut self, id: RequestId) -> Result<Option<TransferCompletion>, TransferError> {
        match self.raw.reclaim_request(id) {
            Some(result) => result.map(Some),
            None => Ok(None),
        }
    }

    pub fn poll_request(
        &mut self,
        id: RequestId,
        cx: &mut Context<'_>,
    ) -> Poll<Result<TransferCompletion, TransferError>> {
        match self.raw.reclaim_request(id) {
            Some(res) => Poll::Ready(res),
            None => {
                self.raw.register_waker(id, cx);
                match self.raw.reclaim_request(id) {
                    Some(res) => Poll::Ready(res),
                    None => Poll::Pending,
                }
            }
        }
    }

    pub fn cancel(&mut self, id: RequestId) -> Result<(), TransferError> {
        self.raw.cancel_request(id)
    }

    pub async fn wait(
        &mut self,
        request: TransferRequest,
    ) -> Result<TransferCompletion, TransferError> {
        let id = self.submit(request)?;
        EndpointRequestFuture { id, endpoint: self }.await
    }

    #[allow(unused)]
    pub(crate) fn with_raw_mut<T: EndpointOp, R>(&mut self, f: impl FnOnce(&mut T) -> R) -> R {
        let d = self.raw.as_mut() as &mut dyn Any;
        f(d.downcast_mut::<T>().expect("Endpoint downcast_mut failed"))
    }

    fn validate_request(&self, request: &TransferRequest) -> Result<(), TransferError> {
        let request_type = match request {
            TransferRequest::Control { .. } => EndpointType::Control,
            TransferRequest::Bulk { .. } => EndpointType::Bulk,
            TransferRequest::Interrupt { .. } => EndpointType::Interrupt,
            TransferRequest::Isochronous { .. } => EndpointType::Isochronous,
        };
        if request_type == self.info.transfer_type {
            Ok(())
        } else {
            Err(TransferError::InvalidEndpoint)
        }
    }
}

struct EndpointRequestFuture<'a> {
    id: RequestId,
    endpoint: &'a mut Endpoint,
}

impl Future for EndpointRequestFuture<'_> {
    type Output = Result<TransferCompletion, TransferError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.endpoint.poll_request(this.id, cx)
    }
}

pub(crate) fn transfer_to_completion(id: RequestId, transfer: Transfer) -> TransferCompletion {
    let iso_packets = match &transfer.kind {
        usb_if::endpoint::TransferKind::Isochronous { packet_lengths } => packet_lengths
            .iter()
            .copied()
            .zip(transfer.iso_packet_actual_lengths.iter().copied())
            .map(|(requested_length, actual_length)| IsoPacketResult {
                requested_length,
                actual_length,
                status: TransferStatus::Completed,
            })
            .collect(),
        _ => Vec::new(),
    };

    TransferCompletion {
        request_id: id,
        status: TransferStatus::Completed,
        actual_length: transfer.transfer_len,
        iso_packets,
    }
}
