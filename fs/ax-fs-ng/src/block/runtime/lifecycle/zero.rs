//! Synchronous range initialization with bounded, exclusively owned DMA reuse.

use alloc::{collections::VecDeque, vec::Vec};

use dma_api::{CpuDmaBuffer, DmaDirection, PreparedDma};
use rdif_block::{
    BlkError, CompletedRequest, OwnedRequest, OwnedRequestBatch, QueueInfo, RequestFlags,
    RequestOp, TransferChunk, TransferPlan,
};

use super::{
    super::{CompletionGroup, dma::prepare_zero},
    BlockDeviceHandle, block_io_error,
    io::{
        SOFTWARE_PIPELINE_WINDOWS, ensure_sleepable, submission_window_limit, take_window,
        transfer_planner,
    },
};
use crate::{
    BlockError, BlockResult,
    block::{BlockRegion, zero_range_bounds},
};

/// Bounds each allocation while allowing several inode tables in one window.
const MAX_ZERO_TRANSFER_BYTES: usize = 1024 * 1024;
/// Includes both outstanding windows and idle reusable payloads.
const MAX_ZERO_DMA_BYTES: usize = 16 * 1024 * 1024;

/// Retains chunk identity until every accepted request has completed.
struct ZeroWindow {
    /// Submission-order geometry used to validate and recycle completions.
    chunks: Vec<TransferChunk>,
    /// Owns every accepted request until the infallible drain consumes it.
    completions: CompletionGroup,
}

/// Validates the full operation before admitting any write. A failure after
/// admission stops new submissions and drains both windows before returning.
pub(super) fn write_zeroes(device: &BlockDeviceHandle, ranges: &[BlockRegion]) -> BlockResult {
    let info = device.inner.selected_queue_info().ok_or(BlockError::Io)?;
    if zero_range_bounds(ranges, info.device.num_blocks)?.is_none() {
        return Ok(());
    }
    if info.device.read_only {
        return Err(BlockError::Unsupported);
    }
    ensure_sleepable()?;
    if info.limits.max_write_zeroes_blocks != 0 {
        return write_native_zeroes(device, info, ranges);
    }
    log::debug!(
        "block DMA zero: device={} ranges={}",
        device.name(),
        ranges.len()
    );
    write_zeroes_dma(device, info, ranges)
}

/// Sends exact, payload-free ranges, independent of all DMA transfer limits.
/// One maintenance request at a time avoids filling a command queue with
/// serial TRIM operations whose deadlines would expire while waiting.
/// The caller has validated every range and a nonzero native block limit.
fn write_native_zeroes(
    device: &BlockDeviceHandle,
    info: QueueInfo,
    ranges: &[BlockRegion],
) -> BlockResult {
    log::debug!(
        "block native zero: device={} ranges={} max_blocks={}",
        device.name(),
        ranges.len(),
        info.limits.max_write_zeroes_blocks,
    );
    for range in ranges {
        let mut lba = range.start_lba;
        while lba < range.end_lba {
            let count =
                (range.end_lba - lba).min(u64::from(info.limits.max_write_zeroes_blocks)) as u32;
            let batch = [OwnedRequest {
                op: RequestOp::WriteZeroes,
                lba,
                block_count: count,
                data: None,
                flags: RequestFlags::NONE,
            }]
            .into_iter()
            .collect();
            let completed = device.submit_batch_owned(batch).map_err(|error| {
                block_io_error(
                    "submit native zero",
                    RequestOp::WriteZeroes,
                    lba,
                    error.error,
                )
            })?;
            let mut result = Ok(());
            // Admission established a sleepable context. Even an error
            // must consume the whole group before range exclusion ends.
            completed.drain_admitted(|completion| {
                let outcome = completion.result.map_err(|error| {
                    block_io_error("complete native zero", RequestOp::WriteZeroes, lba, error)
                });
                if result.is_ok() {
                    result = outcome.and_then(|()| {
                        if completion.data.is_none() {
                            Ok(())
                        } else {
                            Err(BlockError::InvalidState)
                        }
                    });
                }
            });
            result?;
            lba += u64::from(count);
        }
    }
    Ok(())
}

/// Ordinary writes remain the fallback when the device cannot guarantee
/// native read-zero semantics. Accepted requests retain exclusive DMA backing.
fn write_zeroes_dma(
    device: &BlockDeviceHandle,
    info: QueueInfo,
    ranges: &[BlockRegion],
) -> BlockResult {
    let planner = transfer_planner(info, MAX_ZERO_TRANSFER_BYTES).map_err(|error| {
        block_io_error(
            "create zero plan",
            RequestOp::Write,
            ranges[0].start_lba,
            error,
        )
    })?;
    let mut plans: Vec<TransferPlan> = Vec::new();
    plans
        .try_reserve_exact(ranges.len())
        .map_err(|_| BlockError::NoMemory)?;
    for range in ranges.iter().filter(|range| range.num_blocks() != 0) {
        let bytes = usize::try_from(range.num_blocks())
            .ok()
            .and_then(|count| count.checked_mul(info.device.logical_block_size))
            .ok_or(BlockError::InvalidRequest)?;
        plans.push(planner.plan(range.start_lba, bytes).map_err(|error| {
            block_io_error("plan zero range", RequestOp::Write, range.start_lba, error)
        })?);
    }
    let window_limit = submission_window_limit(info)
        .min(MAX_ZERO_DMA_BYTES / SOFTWARE_PIPELINE_WINDOWS / planner.chunk_size());
    let mut reusable = Vec::new();
    reusable
        .try_reserve_exact(window_limit * SOFTWARE_PIPELINE_WINDOWS)
        .map_err(|_| BlockError::NoMemory)?;
    let mut pending = VecDeque::new();
    pending
        .try_reserve_exact(SOFTWARE_PIPELINE_WINDOWS)
        .map_err(|_| BlockError::NoMemory)?;
    let mut chunks = plans.into_iter().flatten().peekable();
    let mut first_error = None;

    while chunks.peek().is_some() || !pending.is_empty() {
        while first_error.is_none()
            && pending.len() < SOFTWARE_PIPELINE_WINDOWS
            && chunks.peek().is_some()
        {
            let result = take_window(&mut chunks, window_limit)
                .and_then(|window| submit_window(device, info, window, &mut reusable));
            match result {
                Ok(window) => pending.push_back(window),
                Err(error) => first_error = Some(error),
            }
        }
        let Some(window) = pending.pop_front() else {
            break;
        };
        // Blocking was checked before admission. Receiving cannot allocate or
        // return early: the caller may release a cache range lock on return.
        let mut chunks = window.chunks.into_iter();
        window.completions.drain_admitted(|completion| {
            let result = recycle_completion(info, chunks.next(), completion, &mut reusable);
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        });
    }
    first_error.map_or(Ok(()), Err)
}

/// Re-prepares only completed ToDevice buffers; no backing is shared by two
/// requests. Dropping a wrong-sized idle buffer before replacement keeps the
/// total allocation count within the two-window memory budget.
fn zero_payload(
    info: QueueInfo,
    len: usize,
    reusable: &mut Vec<CpuDmaBuffer>,
) -> Result<PreparedDma, BlkError> {
    if let Some(index) = reusable.iter().position(|buffer| buffer.len().get() == len) {
        return Ok(reusable.swap_remove(index).prepare_for_device());
    }
    drop(reusable.pop());
    prepare_zero(info.limits, len)
}

/// Transfers a complete window to the runtime, preserving DMA ownership on
/// admission failure; no request in a rejected batch has been accepted.
fn submit_window(
    device: &BlockDeviceHandle,
    info: QueueInfo,
    chunks: Vec<TransferChunk>,
    reusable: &mut Vec<CpuDmaBuffer>,
) -> Result<ZeroWindow, BlockError> {
    let mut requests = OwnedRequestBatch::with_capacity(chunks.len());
    for chunk in &chunks {
        let payload = zero_payload(info, chunk.byte_len, reusable).map_err(|error| {
            block_io_error("prepare zero DMA", RequestOp::Write, chunk.lba, error)
        })?;
        requests.push_back(OwnedRequest {
            op: RequestOp::Write,
            lba: chunk.lba,
            block_count: chunk.block_count,
            data: Some(payload),
            flags: RequestFlags::NONE,
        });
    }
    let completions = device.submit_batch_owned(requests).map_err(|error| {
        block_io_error(
            "submit zero window",
            RequestOp::Write,
            chunks[0].lba,
            error.error,
        )
    })?;
    Ok(ZeroWindow {
        chunks,
        completions,
    })
}

/// Only successful terminal completions can return backing to this call's pool.
/// Device failures are recorded by the caller without interrupting the drain.
fn recycle_completion(
    info: QueueInfo,
    chunk: Option<TransferChunk>,
    completion: CompletedRequest,
    reusable: &mut Vec<CpuDmaBuffer>,
) -> BlockResult {
    let chunk = chunk.ok_or(BlockError::InvalidState)?;
    completion
        .result
        .map_err(|error| block_io_error("complete window", RequestOp::Write, chunk.lba, error))?;
    let payload = completion
        .data
        .ok_or(BlockError::InvalidState)?
        .into_cpu_buffer();
    if payload.len().get() != chunk.byte_len
        || payload.direction() != DmaDirection::ToDevice
        || payload.domain_id() != info.limits.dma.domain()
    {
        return Err(BlockError::InvalidState);
    }
    reusable.push(payload);
    Ok(())
}
