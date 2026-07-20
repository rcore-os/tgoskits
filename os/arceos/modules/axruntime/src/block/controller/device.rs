//! Logical block-device views retained by one controller owner.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_kspin::SpinNoPreempt;
use rdif_block::{
    DeviceInfo, LogicalDeviceId, LogicalDeviceParts, QueueHandle, QueueInfo, QueueLimits,
};

use super::BlockController;
use crate::block::{
    BlockIoStats, BlockServiceError, HardwareQueue,
    quarantine::{QueueQuarantineReservation, quarantine_live_queue},
    statistics::BlockIoCounters,
};

/// Filesystem-facing identity and I/O capability for exactly one logical disk.
#[derive(Clone)]
pub struct BlockDeviceView {
    controller: Arc<BlockController>,
    device_slot: usize,
}

impl BlockDeviceView {
    pub(super) const fn new(controller: Arc<BlockController>, device_slot: usize) -> Self {
        Self {
            controller,
            device_slot,
        }
    }

    /// Returns the controller-local logical device identity.
    pub fn id(&self) -> LogicalDeviceId {
        self.runtime_device().id
    }

    /// Returns the driver-provided name of this logical device.
    pub fn name(&self) -> &str {
        &self.runtime_device().name
    }

    /// Returns immutable geometry for this logical device only.
    pub fn device_info(&self) -> DeviceInfo {
        self.runtime_device().info
    }

    /// Returns immutable request limits shared by this device's queues.
    pub fn queue_limits(&self) -> QueueLimits {
        self.runtime_device().limits
    }

    /// Returns successful I/O counters for this logical device only.
    pub fn io_stats(&self) -> BlockIoStats {
        self.runtime_device().statistics.snapshot()
    }

    pub(in crate::block) fn controller(&self) -> &BlockController {
        &self.controller
    }

    pub(in crate::block) fn runtime_device(&self) -> &RuntimeBlockDevice {
        self.controller
            .devices
            .get(self.device_slot)
            .expect("a block device view must retain a valid controller slot")
    }
}

pub(in crate::block) struct RuntimeBlockDevice {
    pub(super) id: LogicalDeviceId,
    pub(super) name: String,
    pub(super) info: DeviceInfo,
    pub(super) limits: QueueLimits,
    pub(in crate::block) queues: Box<[RuntimeQueue]>,
    pub(in crate::block) dispatch_cursor: AtomicUsize,
    pub(super) statistics: BlockIoCounters,
}

impl RuntimeBlockDevice {
    pub(super) fn new(parts: LogicalDeviceParts, queues: Vec<RuntimeQueue>) -> Self {
        Self {
            id: parts.id,
            name: parts.name,
            info: parts.device_info,
            limits: parts.queue_limits,
            queues: queues.into_boxed_slice(),
            dispatch_cursor: AtomicUsize::new(0),
            statistics: BlockIoCounters::new(),
        }
    }

    pub(in crate::block) fn info(&self) -> DeviceInfo {
        self.info
    }

    pub(in crate::block) fn record_successful_io(
        &self,
        operation: rdif_block::RequestOp,
        byte_len: usize,
    ) {
        self.statistics.record_success(operation, byte_len);
    }
}

pub(in crate::block) enum RuntimeQueue {
    Inline(Box<InlineQueue>),
    Interrupt(Arc<HardwareQueue>),
}

impl RuntimeQueue {
    pub(in crate::block) fn info(&self) -> QueueInfo {
        match self {
            Self::Inline(queue) => queue.info,
            Self::Interrupt(queue) => queue.info(),
        }
    }

    pub(super) fn interrupt_queue(&self) -> Option<&Arc<HardwareQueue>> {
        match self {
            Self::Inline(_) => None,
            Self::Interrupt(queue) => Some(queue),
        }
    }
}

pub(in crate::block) struct InlineQueue {
    pub(in crate::block) info: QueueInfo,
    // Preemption exclusion prevents CPU migration while serializing submit
    // versus teardown without masking IRQs across an inline memory copy.
    pub(in crate::block) queue: SpinNoPreempt<Option<QueueHandle>>,
    quarantine_reservation: SpinNoPreempt<Option<QueueQuarantineReservation>>,
    pub(in crate::block) available: AtomicBool,
}

impl InlineQueue {
    pub(super) fn new(
        queue: QueueHandle,
        quarantine_reservation: QueueQuarantineReservation,
    ) -> Self {
        Self {
            info: queue.info(),
            queue: SpinNoPreempt::new(Some(queue)),
            quarantine_reservation: SpinNoPreempt::new(Some(quarantine_reservation)),
            available: AtomicBool::new(true),
        }
    }

    pub(in crate::block) fn shutdown_after_contract_violation(
        &self,
    ) -> Result<(), rdif_block::BlkError> {
        let queue = self.queue.lock().take();
        let Some(queue) = queue else {
            return Ok(());
        };
        let reservation = self
            .quarantine_reservation
            .lock()
            .take()
            .expect("live inline queue must retain its quarantine reservation");
        crate::block::quarantine::close_or_quarantine(queue, reservation)
    }

    pub(super) fn shutdown_unpublished(&self) -> Result<(), rdif_block::BlkError> {
        self.shutdown_after_contract_violation()
    }

    pub(super) fn close_on_owner(&self) -> Result<(), rdif_block::BlkError> {
        self.available.store(false, Ordering::Release);
        self.shutdown_after_contract_violation()
    }

    pub(super) fn quarantine_on_owner(&self, reason: rdif_block::BlkError) {
        self.available.store(false, Ordering::Release);
        if let Some(queue) = self.queue.lock().take() {
            let reservation = self
                .quarantine_reservation
                .lock()
                .take()
                .expect("live inline queue must retain its quarantine reservation");
            crate::block::quarantine::quarantine_live_queue(queue, reason, reservation);
        }
    }
}

impl Drop for InlineQueue {
    fn drop(&mut self) {
        if let Some(queue) = self.queue.get_mut().take() {
            error!(
                "inline block queue {} dropped before explicit owner close",
                self.info.id
            );
            let reservation = self
                .quarantine_reservation
                .get_mut()
                .take()
                .expect("a live inline queue must retain its quarantine reservation");
            quarantine_live_queue(queue, rdif_block::BlkError::Quarantined, reservation);
        }
    }
}

impl BlockController {
    /// Returns independently addressed logical devices owned by this controller.
    pub fn logical_devices(self: &Arc<Self>) -> Vec<BlockDeviceView> {
        (0..self.devices.len())
            .map(|device_slot| BlockDeviceView::new(Arc::clone(self), device_slot))
            .collect()
    }

    /// Adapts a controller containing exactly one logical device.
    ///
    /// # Errors
    ///
    /// Returns [`BlockServiceError::AmbiguousLogicalDevice`] instead of
    /// selecting the first disk when a controller owns several devices.
    pub fn single_device_view(self: &Arc<Self>) -> Result<BlockDeviceView, BlockServiceError> {
        if self.devices.len() != 1 {
            return Err(BlockServiceError::AmbiguousLogicalDevice {
                device_count: self.devices.len(),
            });
        }
        Ok(BlockDeviceView::new(Arc::clone(self), 0))
    }

    pub(super) fn runtime_queues(&self) -> impl Iterator<Item = &RuntimeQueue> {
        self.devices.iter().flat_map(|device| device.queues.iter())
    }
}
