//! Logical block-device views retained by one controller owner.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{
    mem::ManuallyDrop,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize},
};

use ax_kspin::SpinNoPreempt;
use rdif_block::{
    CompletedRequest, CompletionSink, DeviceInfo, LogicalDeviceId, LogicalDeviceParts,
    OwnedRequest, QueueHandle, QueueInfo, QueueLimits,
};

use super::BlockController;
use crate::block::{BlockIoStats, BlockServiceError, HardwareQueue, statistics::BlockIoCounters};

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
    Interrupt(Pin<&'static HardwareQueue>),
}

impl RuntimeQueue {
    pub(in crate::block) fn info(&self) -> QueueInfo {
        match self {
            Self::Inline(queue) => queue.info,
            Self::Interrupt(queue) => queue.info(),
        }
    }

    pub(super) fn interrupt_queue(&self) -> Option<Pin<&'static HardwareQueue>> {
        match self {
            Self::Inline(_) => None,
            Self::Interrupt(queue) => Some(*queue),
        }
    }
}

pub(in crate::block) struct InlineQueue {
    pub(in crate::block) info: QueueInfo,
    // Preemption exclusion prevents CPU migration while serializing submit
    // versus teardown without masking IRQs across an inline memory copy.
    pub(in crate::block) queue: SpinNoPreempt<QueueHandle>,
    pub(in crate::block) available: AtomicBool,
    rejected_owners: SpinNoPreempt<RejectedOwnerQuarantine>,
}

impl InlineQueue {
    pub(super) fn new(queue: QueueHandle) -> Self {
        Self {
            info: queue.info(),
            queue: SpinNoPreempt::new(queue),
            available: AtomicBool::new(true),
            rejected_owners: SpinNoPreempt::new(RejectedOwnerQuarantine::new()),
        }
    }

    pub(in crate::block) fn retain_rejected_completion(&self, completion: CompletedRequest) {
        self.rejected_owners
            .lock()
            .retain(RejectedInlineOwner::Completion(completion));
    }

    pub(in crate::block) fn retain_rejected_request(&self, request: OwnedRequest) {
        self.rejected_owners
            .lock()
            .retain(RejectedInlineOwner::Request(request));
    }

    pub(in crate::block) fn shutdown_after_contract_violation(
        &self,
    ) -> Result<(), rdif_block::BlkError> {
        // Lock order is queue -> rejected_owners. Submission never holds the
        // quarantine while entering the driver, so teardown cannot deadlock a
        // producer that observed availability before the poison publication.
        let mut queue = self.queue.lock();
        let mut rejected_owners = self.rejected_owners.lock();
        queue.shutdown(&mut *rejected_owners)
    }

    pub(super) fn shutdown_unpublished(&self) -> Result<(), rdif_block::BlkError> {
        self.shutdown_after_contract_violation()
    }
}

impl Drop for InlineQueue {
    fn drop(&mut self) {
        let _ = self
            .queue
            .get_mut()
            .shutdown(self.rejected_owners.get_mut());
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

const INLINE_REJECTED_OWNER_CAPACITY: usize = 2;

enum RejectedInlineOwner {
    Request(OwnedRequest),
    Completion(CompletedRequest),
}

pub(super) struct RejectedOwnerQuarantine {
    owners: [Option<ManuallyDrop<RejectedInlineOwner>>; INLINE_REJECTED_OWNER_CAPACITY],
    len: usize,
}

impl RejectedOwnerQuarantine {
    pub(super) const fn new() -> Self {
        Self {
            owners: [const { None }; INLINE_REJECTED_OWNER_CAPACITY],
            len: 0,
        }
    }

    fn retain(&mut self, owner: RejectedInlineOwner) {
        if self.len == self.owners.len() {
            let _unrepresentable_owner = ManuallyDrop::new(owner);
            panic!("inline block queue fabricated more than one poison request owner");
        }
        self.owners[self.len] = Some(ManuallyDrop::new(owner));
        self.len += 1;
    }
}

impl Drop for RejectedOwnerQuarantine {
    fn drop(&mut self) {
        for slot in &mut self.owners[..self.len] {
            let Some(owner) = slot.take() else {
                continue;
            };
            match ManuallyDrop::into_inner(owner) {
                RejectedInlineOwner::Request(request) => {
                    let _shutdown_lifetime_owner = ManuallyDrop::new(request);
                }
                RejectedInlineOwner::Completion(completion) => {
                    let _shutdown_lifetime_owner = ManuallyDrop::new(completion);
                }
            }
        }
    }
}

impl CompletionSink for RejectedOwnerQuarantine {
    fn complete(&mut self, completion: CompletedRequest) {
        self.retain(RejectedInlineOwner::Completion(completion));
    }
}
