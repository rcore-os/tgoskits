use crate::{BlkError, DeviceInfo, QueueLimits};

#[derive(Debug, Clone, Copy)]
pub struct TransferRuntimeCaps {
    pub max_transfer_bytes: usize,
    pub max_segments: usize,
}

impl TransferRuntimeCaps {
    pub const fn new(max_transfer_bytes: usize, max_segments: usize) -> Self {
        Self {
            max_transfer_bytes,
            max_segments,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferSegment {
    /// Segment byte offset relative to the containing transfer chunk.
    pub byte_offset: usize,
    pub byte_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferChunk {
    pub lba: u64,
    pub block_count: u32,
    pub byte_offset: usize,
    pub byte_len: usize,
    max_segment_size: usize,
}

impl TransferChunk {
    pub fn segments(self) -> TransferSegments {
        TransferSegments {
            remaining_len: self.byte_len,
            byte_offset: 0,
            max_segment_size: self.max_segment_size,
        }
    }
}

pub struct TransferSegments {
    remaining_len: usize,
    byte_offset: usize,
    max_segment_size: usize,
}

impl Iterator for TransferSegments {
    type Item = TransferSegment;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining_len == 0 {
            return None;
        }

        let byte_len = self.remaining_len.min(self.max_segment_size);
        let segment = TransferSegment {
            byte_offset: self.byte_offset,
            byte_len,
        };
        self.byte_offset += byte_len;
        self.remaining_len -= byte_len;
        Some(segment)
    }
}

impl ExactSizeIterator for TransferSegments {
    fn len(&self) -> usize {
        self.remaining_len.div_ceil(self.max_segment_size)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TransferPlanner {
    device: DeviceInfo,
    limits: QueueLimits,
    max_chunk_size: usize,
}

impl TransferPlanner {
    pub fn new(
        device: DeviceInfo,
        limits: QueueLimits,
        caps: TransferRuntimeCaps,
    ) -> Result<Self, BlkError> {
        let max_chunk_size = planned_transfer_size(device, limits, caps)?;

        Ok(Self {
            device,
            limits,
            max_chunk_size,
        })
    }

    pub const fn chunk_size(&self) -> usize {
        self.max_chunk_size
    }

    pub fn plan(&self, lba: u64, byte_len: usize) -> Result<TransferPlan, BlkError> {
        TransferPlan::new(self.device, self.limits, self.max_chunk_size, lba, byte_len)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TransferPlan {
    next_lba: u64,
    byte_offset: usize,
    remaining_bytes: usize,
    block_size: usize,
    max_chunk_size: usize,
    max_segment_size: usize,
}

impl TransferPlan {
    fn new(
        device: DeviceInfo,
        limits: QueueLimits,
        max_chunk_size: usize,
        lba: u64,
        byte_len: usize,
    ) -> Result<Self, BlkError> {
        let block_size = device.logical_block_size;
        if block_size == 0 || byte_len == 0 || !byte_len.is_multiple_of(block_size) {
            return Err(BlkError::InvalidRequest);
        }

        let block_count = byte_len / block_size;
        let block_count_u64 = u64::try_from(block_count).map_err(|_| BlkError::InvalidRequest)?;
        if lba >= device.num_blocks
            || lba
                .checked_add(block_count_u64)
                .is_none_or(|end| end > device.num_blocks)
        {
            return Err(BlkError::InvalidBlockIndex(lba));
        }

        Ok(Self {
            next_lba: lba,
            byte_offset: 0,
            remaining_bytes: byte_len,
            block_size,
            max_chunk_size,
            max_segment_size: limits.max_segment_size,
        })
    }
}

fn planned_transfer_size(
    device: DeviceInfo,
    limits: QueueLimits,
    caps: TransferRuntimeCaps,
) -> Result<usize, BlkError> {
    let block_size = device.logical_block_size;
    let max_segments = limits.max_segments.min(caps.max_segments);
    if block_size == 0
        || limits.max_blocks_per_request == 0
        || max_segments == 0
        || limits.max_segment_size == 0
        || caps.max_transfer_bytes == 0
    {
        return Err(BlkError::InvalidRequest);
    }

    let max_by_blocks = block_size.saturating_mul(limits.max_blocks_per_request as usize);
    let max_by_segments = limits.max_segment_size.saturating_mul(max_segments);
    let max_chunk_size = [max_by_blocks, max_by_segments, caps.max_transfer_bytes]
        .into_iter()
        .min()
        .ok_or(BlkError::InvalidRequest)?;
    let max_chunk_size = align_down(max_chunk_size, block_size);
    if max_chunk_size < block_size {
        return Err(BlkError::InvalidRequest);
    }
    Ok(max_chunk_size)
}

impl Iterator for TransferPlan {
    type Item = TransferChunk;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining_bytes == 0 {
            return None;
        }

        let byte_len = self.remaining_bytes.min(self.max_chunk_size);
        let block_count = byte_len / self.block_size;
        let block_count_u32 = block_count as u32;
        let chunk = TransferChunk {
            lba: self.next_lba,
            block_count: block_count_u32,
            byte_offset: self.byte_offset,
            byte_len,
            max_segment_size: self.max_segment_size,
        };

        self.next_lba += block_count as u64;
        self.byte_offset += byte_len;
        self.remaining_bytes -= byte_len;
        Some(chunk)
    }
}

fn align_down(value: usize, align: usize) -> usize {
    value / align * align
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::{QueueInfo, RequestFlags};

    fn queue_info_with(limits: QueueLimits) -> QueueInfo {
        QueueInfo {
            id: 0,
            device: DeviceInfo::new(64, 512),
            limits,
        }
    }

    fn queue_limits(
        max_blocks_per_request: u32,
        max_segments: usize,
        max_segment_size: usize,
    ) -> QueueLimits {
        QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 512,
            max_blocks_per_request,
            max_segments,
            max_segment_size,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        }
    }

    fn test_runtime_caps() -> TransferRuntimeCaps {
        TransferRuntimeCaps {
            max_transfer_bytes: 16 * 1024,
            max_segments: 16,
        }
    }

    fn chunk_summary(chunks: &[TransferChunk]) -> Vec<(u64, u32, usize, usize, usize)> {
        chunks
            .iter()
            .map(|chunk| {
                let segments = chunk.segments();
                (
                    chunk.lba,
                    chunk.block_count,
                    chunk.byte_offset,
                    chunk.byte_len,
                    segments.len(),
                )
            })
            .collect()
    }

    #[test]
    fn simple_limits_allow_single_block_transfers() {
        let info = queue_info_with(QueueLimits::simple(512, u64::MAX));
        let planner = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();
        let plan = planner.plan(0, 2048).unwrap();
        let chunks: Vec<_> = plan.collect();

        assert_eq!(planner.chunk_size(), 512);
        assert_eq!(
            chunk_summary(&chunks),
            [
                (0, 1, 0, 512, 1),
                (1, 1, 512, 512, 1),
                (2, 1, 1024, 512, 1),
                (3, 1, 1536, 512, 1),
            ]
        );
    }

    #[test]
    fn transfer_plan_chunks_by_runtime_cap() {
        let info = queue_info_with(queue_limits(16, 4, 4096));
        let planner = TransferPlanner::new(
            info.device,
            info.limits,
            TransferRuntimeCaps {
                max_transfer_bytes: 2048,
                max_segments: 16,
            },
        )
        .unwrap();
        let plan = planner.plan(4, 5120).unwrap();
        let chunks: Vec<_> = plan.collect();

        assert_eq!(planner.chunk_size(), 2048);
        assert_eq!(
            chunk_summary(&chunks),
            [
                (4, 4, 0, 2048, 1),
                (8, 4, 2048, 2048, 1),
                (12, 2, 4096, 1024, 1),
            ]
        );
    }

    #[test]
    fn transfer_chunk_segments_split_by_hard_segment_size() {
        let info = queue_info_with(queue_limits(16, 4, 1024));
        let planner = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();
        let mut plan = planner.plan(0, 4096).unwrap();
        let chunk = plan.next().unwrap();
        let segment_iter = chunk.segments();
        assert_eq!(segment_iter.len(), 4);
        let segments: Vec<_> = segment_iter.collect();

        assert_eq!(
            segments,
            [
                TransferSegment {
                    byte_offset: 0,
                    byte_len: 1024,
                },
                TransferSegment {
                    byte_offset: 1024,
                    byte_len: 1024,
                },
                TransferSegment {
                    byte_offset: 2048,
                    byte_len: 1024,
                },
                TransferSegment {
                    byte_offset: 3072,
                    byte_len: 1024,
                },
            ]
        );
        assert!(plan.next().is_none());
    }

    #[test]
    fn transfer_plan_clamps_to_hard_block_count() {
        let info = queue_info_with(queue_limits(4, 8, 4096));
        let planner = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();
        let plan = planner.plan(0, 5120).unwrap();
        let chunks: Vec<_> = plan.collect();

        assert_eq!(planner.chunk_size(), 2048);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.byte_len)
                .collect::<Vec<_>>(),
            [2048, 2048, 1024]
        );
    }

    #[test]
    fn transfer_plan_clamps_to_runtime_limits() {
        let info = queue_info_with(queue_limits(16, 8, 2048));
        let planner = TransferPlanner::new(
            info.device,
            info.limits,
            TransferRuntimeCaps {
                max_transfer_bytes: 4096,
                max_segments: 1,
            },
        )
        .unwrap();
        let plan = planner.plan(0, 4096).unwrap();
        let chunks: Vec<_> = plan.collect();

        assert_eq!(planner.chunk_size(), 2048);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.byte_len)
                .collect::<Vec<_>>(),
            [2048, 2048]
        );
    }

    #[test]
    fn transfer_planner_rejects_too_small_runtime_cap() {
        let info = queue_info_with(queue_limits(16, 8, 2048));

        assert_eq!(
            TransferPlanner::new(
                info.device,
                info.limits,
                TransferRuntimeCaps {
                    max_transfer_bytes: 511,
                    max_segments: 1,
                },
            )
            .unwrap_err(),
            BlkError::InvalidRequest
        );
    }

    #[test]
    fn transfer_planner_does_not_depend_on_queue_identity() {
        let mut info = queue_info_with(queue_limits(16, 8, 2048));
        let first = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();
        info.id = 7;
        let second = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();

        assert_eq!(first.chunk_size(), second.chunk_size());
    }

    #[test]
    fn transfer_planner_checks_range_when_creating_plan() {
        let info = queue_info_with(QueueLimits::simple(512, u64::MAX));
        let planner = TransferPlanner::new(info.device, info.limits, test_runtime_caps()).unwrap();

        assert_eq!(
            planner.plan(63, 1024).unwrap_err(),
            BlkError::InvalidBlockIndex(63)
        );
    }
}
