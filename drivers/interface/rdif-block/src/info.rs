use dma_api::DmaDomainId;

use crate::{irq::IdList, request::RequestFlags};

/// Default absolute watchdog budget for one accepted hardware request.
pub const DEFAULT_REQUEST_TIMEOUT_NS: u64 = 30_000_000_000;

/// How a queue returns terminal request ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    /// Submission with [`crate::RequestId::INLINE`] completes before
    /// [`crate::IQueue::submit_owned`] returns.
    Inline,
    /// Submission completion is reported after a device interrupt event.
    Interrupt {
        /// Logical interrupt source IDs that can report this queue.
        sources: IdList,
    },
}

/// How accepted requests enter and advance one queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueExecution {
    /// A software device completes ownership before submission returns.
    Inline,
    /// One owner-side service context may maintain multiple generation-tagged
    /// requests in flight on the hardware queue.
    Tagged,
    /// A single task-side service context must serialize queue progression.
    Serialized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    pub num_blocks: u64,
    pub logical_block_size: usize,
    pub read_only: bool,
    pub name: Option<&'static str>,
    pub vendor: Option<&'static str>,
    pub model: Option<&'static str>,
}

impl DeviceInfo {
    pub const fn new(num_blocks: u64, logical_block_size: usize) -> Self {
        Self {
            num_blocks,
            logical_block_size,
            read_only: false,
            name: None,
            vendor: None,
            model: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLimits {
    pub dma_mask: u64,
    pub dma_domain: DmaDomainId,
    pub dma_alignment: usize,
    pub max_inflight: usize,
    pub max_blocks_per_request: u32,
    pub max_segments: usize,
    pub max_segment_size: usize,
    /// Monotonic-time budget after hardware acceptance. A zero budget is
    /// invalid for interrupt queues because it cannot protect DMA ownership.
    pub request_timeout_ns: u64,
    pub supported_flags: RequestFlags,
    pub supports_flush: bool,
    pub supports_discard: bool,
    pub supports_write_zeroes: bool,
}

impl QueueLimits {
    pub const fn simple(logical_block_size: usize, dma_mask: u64) -> Self {
        Self {
            dma_mask,
            dma_domain: DmaDomainId::legacy_global(),
            dma_alignment: logical_block_size,
            max_inflight: 1,
            max_blocks_per_request: 1,
            max_segments: 1,
            max_segment_size: logical_block_size,
            request_timeout_ns: DEFAULT_REQUEST_TIMEOUT_NS,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueInfo {
    pub id: usize,
    pub device: DeviceInfo,
    pub limits: QueueLimits,
    pub kind: QueueKind,
    pub execution: QueueExecution,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interrupt_kind(source_id: usize) -> QueueKind {
        let mut sources = IdList::none();
        sources.insert(source_id);
        QueueKind::Interrupt { sources }
    }

    #[test]
    fn completion_kind_and_execution_contract_are_consistent() {
        let tagged_interrupt = QueueInfo {
            id: 0,
            device: DeviceInfo::new(8, 512),
            limits: QueueLimits::simple(512, u64::MAX),
            kind: interrupt_kind(1),
            execution: QueueExecution::Tagged,
        };
        let serialized_interrupt = QueueInfo {
            execution: QueueExecution::Serialized,
            ..tagged_interrupt
        };
        let inline = QueueInfo {
            kind: QueueKind::Inline,
            execution: QueueExecution::Inline,
            ..tagged_interrupt
        };

        assert_eq!(tagged_interrupt.kind, interrupt_kind(1));
        assert_eq!(tagged_interrupt.execution, QueueExecution::Tagged);
        assert_eq!(serialized_interrupt.kind, interrupt_kind(1));
        assert_eq!(serialized_interrupt.execution, QueueExecution::Serialized);
        assert_eq!(inline.kind, QueueKind::Inline);
        assert_eq!(inline.execution, QueueExecution::Inline);
    }

    #[test]
    fn simple_queue_declares_an_absolute_watchdog_budget() {
        let limits = QueueLimits::simple(512, u64::MAX);

        assert_eq!(limits.request_timeout_ns, 30_000_000_000);
    }
}
