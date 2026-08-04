use alloc::{collections::VecDeque, vec::Vec};
use core::iter::Peekable;

use ax_errno::{AxError, AxResult};
use log::warn;
use rdif_block::{
    BlkError, CompletedRequest, OwnedRequest, OwnedRequestBatch, QueueInfo, RequestFlags,
    RequestOp, TransferChunk, TransferPlan, TransferPlanner, TransferRuntimeCaps,
};

#[cfg(any(feature = "ext4", feature = "fat"))]
use super::super::dma::prepare_write;
use super::{super::dma::prepare_read, BlockDeviceHandle, block_io_error, request_cannot_block};

const MAX_RUNTIME_TRANSFER_BYTES: usize = 4 * 1024 * 1024;
const SOFTWARE_PIPELINE_WINDOWS: usize = 2;

struct ReadWindow {
    chunks: Vec<TransferChunk>,
    completions: super::super::CompletionGroup,
}

#[cfg(any(feature = "ext4", feature = "fat"))]
struct WriteWindow {
    chunks: Vec<TransferChunk>,
    completions: super::super::CompletionGroup,
}

pub(super) fn read_blocks(
    device: &BlockDeviceHandle,
    block_id: u64,
    buffer: &mut [u8],
) -> AxResult {
    if buffer.is_empty() {
        return Ok(());
    }
    ensure_sleepable()?;
    let info = device.inner.selected_queue_info().ok_or(AxError::Io)?;
    let mut plan = transfer_plan(info, block_id, buffer.len(), RequestOp::Read)?.peekable();
    let window_limit = submission_window_limit(info);
    let mut pending = VecDeque::with_capacity(SOFTWARE_PIPELINE_WINDOWS);

    while plan.peek().is_some() || !pending.is_empty() {
        while pending.len() < SOFTWARE_PIPELINE_WINDOWS && plan.peek().is_some() {
            pending.push_back(submit_read_window(
                device,
                info,
                take_window(&mut plan, window_limit)?,
            )?);
        }
        let window = pending.pop_front().ok_or(AxError::BadState)?;
        let first_lba = window.chunks[0].lba;
        let completions = window
            .completions
            .recv()
            .map_err(|error| block_io_error("receive window", RequestOp::Read, first_lba, error))?;
        complete_read_window(&window.chunks, completions, buffer)?;
    }
    Ok(())
}

#[cfg(any(feature = "ext4", feature = "fat"))]
pub(super) fn write_blocks(device: &BlockDeviceHandle, block_id: u64, buffer: &[u8]) -> AxResult {
    if buffer.is_empty() {
        return Ok(());
    }
    ensure_sleepable()?;
    let info = device.inner.selected_queue_info().ok_or(AxError::Io)?;
    let mut plan = transfer_plan(info, block_id, buffer.len(), RequestOp::Write)?.peekable();
    let window_limit = submission_window_limit(info);
    let mut pending = VecDeque::with_capacity(SOFTWARE_PIPELINE_WINDOWS);

    while plan.peek().is_some() || !pending.is_empty() {
        while pending.len() < SOFTWARE_PIPELINE_WINDOWS && plan.peek().is_some() {
            pending.push_back(submit_write_window(
                device,
                info,
                take_window(&mut plan, window_limit)?,
                buffer,
            )?);
        }
        let window = pending.pop_front().ok_or(AxError::BadState)?;
        let first_lba = window.chunks[0].lba;
        let completions = window.completions.recv().map_err(|error| {
            block_io_error("receive window", RequestOp::Write, first_lba, error)
        })?;
        complete_write_window(&window.chunks, completions)?;
    }
    Ok(())
}

fn submit_read_window(
    device: &BlockDeviceHandle,
    info: QueueInfo,
    chunks: Vec<TransferChunk>,
) -> Result<ReadWindow, AxError> {
    let first_lba = chunks[0].lba;
    let requests = prepare_read_requests(info, &chunks)?;
    let completions = device.submit_batch_owned(requests).map_err(|error| {
        block_io_error("submit window", RequestOp::Read, first_lba, error.error)
    })?;
    Ok(ReadWindow {
        chunks,
        completions,
    })
}

#[cfg(any(feature = "ext4", feature = "fat"))]
fn submit_write_window(
    device: &BlockDeviceHandle,
    info: QueueInfo,
    chunks: Vec<TransferChunk>,
    buffer: &[u8],
) -> Result<WriteWindow, AxError> {
    let first_lba = chunks[0].lba;
    let requests = prepare_write_requests(info, &chunks, buffer)?;
    let completions = device.submit_batch_owned(requests).map_err(|error| {
        block_io_error("submit window", RequestOp::Write, first_lba, error.error)
    })?;
    Ok(WriteWindow {
        chunks,
        completions,
    })
}

fn transfer_plan(
    info: QueueInfo,
    block_id: u64,
    byte_len: usize,
    op: RequestOp,
) -> Result<TransferPlan, AxError> {
    let boundary_cap = info
        .limits
        .segment_boundary
        .unwrap_or(MAX_RUNTIME_TRANSFER_BYTES);
    let planner = TransferPlanner::new(
        info.device,
        info.limits,
        TransferRuntimeCaps::new(MAX_RUNTIME_TRANSFER_BYTES.min(boundary_cap), 1),
    )
    .map_err(|error| block_io_error("create transfer plan", op, block_id, error))?;
    planner
        .plan(block_id, byte_len)
        .map_err(|error| block_io_error("plan transfer", op, block_id, error))
}

fn submission_window_limit(info: QueueInfo) -> usize {
    info.limits
        .max_inflight
        .min(info.limits.max_submit_batch)
        .max(1)
}

fn take_window(
    plan: &mut Peekable<TransferPlan>,
    limit: usize,
) -> Result<Vec<TransferChunk>, AxError> {
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(limit)
        .map_err(|_| AxError::NoMemory)?;
    chunks.extend(plan.by_ref().take(limit));
    if chunks.is_empty() {
        return Err(AxError::InvalidInput);
    }
    Ok(chunks)
}

fn prepare_read_requests(
    info: QueueInfo,
    chunks: &[TransferChunk],
) -> Result<OwnedRequestBatch, AxError> {
    let mut requests = OwnedRequestBatch::with_capacity(chunks.len());
    for chunk in chunks {
        let data = prepare_read(info.limits, chunk.byte_len)
            .map_err(|error| block_io_error("prepare DMA", RequestOp::Read, chunk.lba, error))?;
        requests.push_back(OwnedRequest {
            op: RequestOp::Read,
            lba: chunk.lba,
            block_count: chunk.block_count,
            data: Some(data),
            flags: RequestFlags::NONE,
        });
    }
    Ok(requests)
}

#[cfg(any(feature = "ext4", feature = "fat"))]
fn prepare_write_requests(
    info: QueueInfo,
    chunks: &[TransferChunk],
    buffer: &[u8],
) -> Result<OwnedRequestBatch, AxError> {
    let mut requests = OwnedRequestBatch::with_capacity(chunks.len());
    for chunk in chunks {
        let range = chunk.byte_offset..chunk.byte_offset + chunk.byte_len;
        let data = prepare_write(info.limits, &buffer[range])
            .map_err(|error| block_io_error("prepare DMA", RequestOp::Write, chunk.lba, error))?;
        requests.push_back(OwnedRequest {
            op: RequestOp::Write,
            lba: chunk.lba,
            block_count: chunk.block_count,
            data: Some(data),
            flags: RequestFlags::NONE,
        });
    }
    Ok(requests)
}

fn complete_read_window(
    chunks: &[TransferChunk],
    completions: Vec<CompletedRequest>,
    buffer: &mut [u8],
) -> AxResult {
    if completions.len() != chunks.len() {
        return Err(block_io_error(
            "match completion window",
            RequestOp::Read,
            chunks[0].lba,
            BlkError::Io,
        ));
    }

    let mut first_error = None;
    for (chunk, completion) in chunks.iter().zip(completions) {
        if let Err(error) = completion.result {
            warn!(
                "block Read completion at LBA {} reported {error:?}",
                chunk.lba
            );
            first_error.get_or_insert((chunk.lba, error));
            continue;
        }
        let Some(data) = completion.data else {
            warn!(
                "block Read completion at LBA {} returned no DMA data",
                chunk.lba
            );
            first_error.get_or_insert((chunk.lba, BlkError::Io));
            continue;
        };
        if data.len().get() != chunk.byte_len {
            warn!(
                "block Read completion at LBA {} returned {} bytes, expected {}",
                chunk.lba,
                data.len(),
                chunk.byte_len
            );
            first_error.get_or_insert((chunk.lba, BlkError::Io));
            continue;
        }
        let range = chunk.byte_offset..chunk.byte_offset + chunk.byte_len;
        data.copy_from_device_to_slice(&mut buffer[range]);
    }

    if let Some((lba, error)) = first_error {
        Err(block_io_error(
            "complete window",
            RequestOp::Read,
            lba,
            error,
        ))
    } else {
        Ok(())
    }
}

#[cfg(any(feature = "ext4", feature = "fat"))]
fn complete_write_window(chunks: &[TransferChunk], completions: Vec<CompletedRequest>) -> AxResult {
    if completions.len() != chunks.len() {
        return Err(block_io_error(
            "match completion window",
            RequestOp::Write,
            chunks[0].lba,
            BlkError::Io,
        ));
    }

    let mut first_error = None;
    for (chunk, completion) in chunks.iter().zip(completions) {
        if let Err(error) = completion.result {
            first_error.get_or_insert((chunk.lba, error));
        }
    }
    if let Some((lba, error)) = first_error {
        Err(block_io_error(
            "complete window",
            RequestOp::Write,
            lba,
            error,
        ))
    } else {
        Ok(())
    }
}

fn ensure_sleepable() -> AxResult {
    if request_cannot_block() {
        Err(AxError::WouldBlock)
    } else {
        Ok(())
    }
}
