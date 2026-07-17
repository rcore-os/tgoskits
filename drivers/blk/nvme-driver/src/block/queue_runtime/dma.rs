//! Request DMA ownership transitions.

use dma_api::{DmaDirection, PreparedDma};
use rdif_block::{BlkError, OwnedRequest, RequestId, RequestOp, SubmitError};

pub(in crate::block) fn prepare_request_dma(
    id: RequestId,
    mut request: OwnedRequest,
) -> Result<(OwnedRequest, Option<PreparedDma>), SubmitError> {
    let expected_direction = match request.op {
        RequestOp::Read => Some(DmaDirection::FromDevice),
        RequestOp::Write => Some(DmaDirection::ToDevice),
        RequestOp::Flush | RequestOp::Discard | RequestOp::WriteZeroes => None,
    };
    let Some(expected_direction) = expected_direction else {
        return Ok((request, None));
    };
    let Some(data) = request.data.take() else {
        return Err(SubmitError::new(id, BlkError::InvalidRequest, request));
    };
    if !dma_direction_supports(data.direction(), expected_direction)
        || data.domain_id() != dma_api::DmaDomainId::legacy_global()
    {
        request.data = Some(data);
        return Err(SubmitError::new(id, BlkError::InvalidRequest, request));
    }
    Ok((request, Some(data.prepare_for_device())))
}

fn dma_direction_supports(actual: DmaDirection, expected: DmaDirection) -> bool {
    actual == expected || actual == DmaDirection::Bidirectional
}

pub(super) fn restore_prepared_dma(
    mut request: OwnedRequest,
    prepared: Option<PreparedDma>,
) -> OwnedRequest {
    if let Some(prepared) = prepared {
        request.data = Some(prepared.into_cpu_buffer());
    }
    request
}
