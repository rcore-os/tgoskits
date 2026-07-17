//! Namespace geometry and queue-limit publication after Ready.

use rdif_block::{DeviceInfo, QueueLimits, RequestFlags};

use crate::Namespace;

pub(super) fn device_info(name: &'static str, namespace: Namespace) -> DeviceInfo {
    DeviceInfo {
        name: Some(name),
        model: Some("nvme"),
        ..DeviceInfo::new(namespace.lba_count, namespace.lba_size)
    }
}

pub(super) fn limits(
    dma_mask: u64,
    page_size: usize,
    controller_max_transfer_bytes: Option<usize>,
    namespace: Namespace,
    max_inflight: usize,
) -> QueueLimits {
    let lba_size = namespace.lba_size.max(1);
    let dma_alignment = page_size.max(lba_size);
    let prp_entries = page_size / core::mem::size_of::<u64>();
    let prp_capacity_bytes = page_size.saturating_mul(prp_entries + 1);
    let max_bytes = controller_max_transfer_bytes
        .map_or(prp_capacity_bytes, |max_transfer| {
            prp_capacity_bytes.min(max_transfer)
        })
        .max(lba_size);
    let max_blocks = max_bytes
        .checked_div(lba_size)
        .unwrap_or(1)
        .max(1)
        .min(u16::MAX as usize + 1) as u32;
    let max_bytes = (max_blocks as usize).saturating_mul(lba_size);
    QueueLimits {
        dma_mask,
        dma_domain: dma_api::DmaDomainId::legacy_global(),
        dma_alignment,
        max_inflight: max_inflight.max(1),
        max_blocks_per_request: max_blocks,
        // RDIF 0.12 submits one contiguous owned DMA buffer. NVMe may encode
        // that single segment through multiple PRP entries internally.
        max_segments: 1,
        max_segment_size: max_bytes,
        request_timeout_ns: rdif_block::DEFAULT_REQUEST_TIMEOUT_NS,
        supported_flags: RequestFlags::NONE,
        // Do not advertise flush until the driver plumbs a reliable capability
        // check from Identify/Feature data. Some QEMU NVMe backends reject the
        // Flush command with "Invalid Field", which must not surface as fsync
        // I/O errors.
        supports_flush: false,
        supports_discard: false,
        supports_write_zeroes: false,
    }
}
