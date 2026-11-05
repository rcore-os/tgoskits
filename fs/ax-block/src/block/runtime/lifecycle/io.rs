use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::iter::Peekable;

use log::warn;
use rdif_block::{
    BlkError, CompletedRequest, OwnedRequest, OwnedRequestBatch, QueueInfo, RequestFlags,
    RequestOp, TransferChunk, TransferPlan, TransferPlanner, TransferRuntimeCaps,
};

#[cfg(feature = "cache")]
use super::super::dma::prepare_write;
use super::{super::dma::prepare_read, DeviceRef, block_io_error, request_cannot_block};
use crate::{BlockError, BlockResult, access::AccessOwner};

const MAX_RUNTIME_TRANSFER_BYTES: usize = 4 * 1024 * 1024;
pub(super) const SOFTWARE_PIPELINE_WINDOWS: usize = 2;

struct ReadWindow {
    chunks: Vec<TransferChunk>,
    completions: super::super::CompletionGroup,
}

#[cfg(feature = "cache")]
struct WriteWindow {
    chunks: Vec<TransferChunk>,
    completions: super::super::CompletionGroup,
}

pub(super) fn read_blocks(
    device: &DeviceRef,
    block_id: u64,
    buffer: &mut [u8],
    owner: Arc<AccessOwner>,
) -> BlockResult {
    if buffer.is_empty() {
        return Ok(());
    }
    ensure_sleepable()?;
    let info = device.inner.selected_queue_info().ok_or(BlockError::Io)?;
    let mut plan = transfer_plan(info, block_id, buffer.len(), RequestOp::Read)?.peekable();
    let window_limit = submission_window_limit(info);
    let mut pending = VecDeque::with_capacity(SOFTWARE_PIPELINE_WINDOWS);

    let mut first_error = None;
    while plan.peek().is_some() || !pending.is_empty() {
        while first_error.is_none()
            && pending.len() < SOFTWARE_PIPELINE_WINDOWS
            && plan.peek().is_some()
        {
            let result = take_window(&mut plan, window_limit)
                .and_then(|chunks| submit_read_window(device, info, chunks, &owner));
            match result {
                Ok(window) => pending.push_back(window),
                Err(error) => first_error = Some(error),
            }
        }
        let Some(window) = pending.pop_front() else {
            break;
        };
        if let Err(error) = complete_read_window(&window.chunks, window.completions, buffer) {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(feature = "cache")]
pub(super) fn write_blocks(
    device: &DeviceRef,
    block_id: u64,
    buffer: &[u8],
    owner: Arc<AccessOwner>,
) -> BlockResult {
    write_blocks_with_flags(device, block_id, buffer, RequestFlags::NONE, owner)
}

#[cfg(feature = "write-zeroes")]
pub(super) fn write_blocks_fua(
    device: &DeviceRef,
    block_id: u64,
    buffer: &[u8],
    owner: Arc<AccessOwner>,
) -> BlockResult {
    write_blocks_with_flags(device, block_id, buffer, RequestFlags::FUA, owner)
}

#[cfg(feature = "cache")]
fn write_blocks_with_flags(
    device: &DeviceRef,
    block_id: u64,
    buffer: &[u8],
    flags: RequestFlags,
    owner: Arc<AccessOwner>,
) -> BlockResult {
    if buffer.is_empty() {
        return Ok(());
    }
    ensure_sleepable()?;
    let info = device.inner.selected_queue_info().ok_or(BlockError::Io)?;
    let mut plan = transfer_plan(info, block_id, buffer.len(), RequestOp::Write)?.peekable();
    let window_limit = submission_window_limit(info);
    let mut pending = VecDeque::with_capacity(SOFTWARE_PIPELINE_WINDOWS);
    let mut first_error = None;

    while plan.peek().is_some() || !pending.is_empty() {
        while first_error.is_none()
            && pending.len() < SOFTWARE_PIPELINE_WINDOWS
            && plan.peek().is_some()
        {
            let window = take_window(&mut plan, window_limit).and_then(|chunks| {
                submit_write_window(device, info, chunks, buffer, flags, &owner)
            });
            match window {
                Ok(window) => pending.push_back(window),
                Err(error) => first_error = Some(error),
            }
        }
        let Some(window) = pending.pop_front() else {
            break;
        };
        let result = complete_write_window(&window.chunks, window.completions);
        if let Err(error) = result
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn submit_read_window(
    device: &DeviceRef,
    info: QueueInfo,
    chunks: Vec<TransferChunk>,
    owner: &Arc<AccessOwner>,
) -> Result<ReadWindow, BlockError> {
    let first_lba = chunks[0].lba;
    let requests = prepare_read_requests(info, &chunks)?;
    let completions = device
        .submit_batch_owned_retained(requests, Some(owner.clone()))
        .map_err(|error| {
            block_io_error("submit window", RequestOp::Read, first_lba, error.error)
        })?;
    Ok(ReadWindow {
        chunks,
        completions,
    })
}

#[cfg(feature = "cache")]
fn submit_write_window(
    device: &DeviceRef,
    info: QueueInfo,
    chunks: Vec<TransferChunk>,
    buffer: &[u8],
    flags: RequestFlags,
    owner: &Arc<AccessOwner>,
) -> Result<WriteWindow, BlockError> {
    let first_lba = chunks[0].lba;
    let requests = prepare_write_requests(info, &chunks, buffer, flags)?;
    let completions = device
        .submit_batch_owned_retained(requests, Some(owner.clone()))
        .map_err(|error| {
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
) -> Result<TransferPlan, BlockError> {
    let planner = transfer_planner(info, MAX_RUNTIME_TRANSFER_BYTES)
        .map_err(|error| block_io_error("create transfer plan", op, block_id, error))?;
    planner
        .plan(block_id, byte_len)
        .map_err(|error| block_io_error("plan transfer", op, block_id, error))
}

/// Applies the same DMA boundary and device limits to ordinary and zero writes.
pub(super) fn transfer_planner(
    info: QueueInfo,
    maximum_bytes: usize,
) -> Result<TransferPlanner, BlkError> {
    let boundary_cap = info
        .limits
        .dma
        .constraints()
        .boundary
        .unwrap_or(maximum_bytes);
    TransferPlanner::new(
        info.device,
        info.limits,
        TransferRuntimeCaps::new(maximum_bytes.min(boundary_cap), 1),
    )
}

pub(super) fn submission_window_limit(info: QueueInfo) -> usize {
    info.limits
        .max_inflight
        .min(info.limits.max_submit_batch)
        .max(1)
}

pub(super) fn take_window(
    plan: &mut Peekable<impl Iterator<Item = TransferChunk>>,
    limit: usize,
) -> Result<Vec<TransferChunk>, BlockError> {
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(limit)
        .map_err(|_| BlockError::NoMemory)?;
    chunks.extend(plan.by_ref().take(limit));
    if chunks.is_empty() {
        return Err(BlockError::InvalidRequest);
    }
    Ok(chunks)
}

fn prepare_read_requests(
    info: QueueInfo,
    chunks: &[TransferChunk],
) -> Result<OwnedRequestBatch, BlockError> {
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

#[cfg(feature = "cache")]
fn prepare_write_requests(
    info: QueueInfo,
    chunks: &[TransferChunk],
    buffer: &[u8],
    flags: RequestFlags,
) -> Result<OwnedRequestBatch, BlockError> {
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
            flags,
        });
    }
    Ok(requests)
}

fn complete_read_window(
    chunks: &[TransferChunk],
    completions: super::super::CompletionGroup,
    buffer: &mut [u8],
) -> BlockResult {
    let mut remaining = chunks.iter();
    let mut first_error = None;
    // No allocation or fallible sleepability check may discard accepted I/O.
    completions.drain_admitted(|completion| {
        let result = match remaining.next() {
            Some(chunk) => complete_read_chunk(chunk, completion, buffer),
            None => Err(BlockError::InvalidState),
        };
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    });
    if remaining.next().is_some() {
        first_error.get_or_insert(BlockError::InvalidState);
    }
    first_error.map_or(Ok(()), Err)
}

fn complete_read_chunk(
    chunk: &TransferChunk,
    completion: CompletedRequest,
    buffer: &mut [u8],
) -> BlockResult {
    if let Err(error) = completion.result {
        warn!(
            "block Read completion at LBA {} reported {error:?}",
            chunk.lba
        );
        return Err(block_io_error(
            "complete window",
            RequestOp::Read,
            chunk.lba,
            error,
        ));
    }
    let data = completion.data.ok_or_else(|| {
        block_io_error("complete window", RequestOp::Read, chunk.lba, BlkError::Io)
    })?;
    if data.len().get() != chunk.byte_len {
        return Err(block_io_error(
            "complete window",
            RequestOp::Read,
            chunk.lba,
            BlkError::Io,
        ));
    }
    let range = chunk.byte_offset..chunk.byte_offset + chunk.byte_len;
    data.copy_to_slice_cpu(&mut buffer[range]);
    Ok(())
}

#[cfg(feature = "cache")]
fn complete_write_window(
    chunks: &[TransferChunk],
    completions: super::super::CompletionGroup,
) -> BlockResult {
    let mut remaining = chunks.iter();
    let mut first_error = None;
    completions.drain_admitted(|completion| {
        let result = match remaining.next() {
            Some(chunk) => completion.result.map_err(|error| {
                block_io_error("complete window", RequestOp::Write, chunk.lba, error)
            }),
            None => Err(BlockError::InvalidState),
        };
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    });
    if remaining.next().is_some() {
        first_error.get_or_insert(BlockError::InvalidState);
    }
    first_error.map_or(Ok(()), Err)
}

pub(super) fn ensure_sleepable() -> BlockResult {
    if request_cannot_block() {
        Err(BlockError::WouldBlock)
    } else {
        Ok(())
    }
}
