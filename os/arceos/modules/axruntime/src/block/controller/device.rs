//! Logical block-device views retained by one controller owner.

use alloc::{boxed::Box, collections::VecDeque, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_kspin::SpinNoPreempt;
use rdif_block::{
    DeviceInfo, LogicalDeviceId, LogicalDeviceParts, QueueHandle, QueueInfo, QueueLimits, RequestOp,
};

use super::BlockController;
use crate::block::{
    BlockIoStats, BlockServiceError, HardwareQueue, HardwareQueueError, RequestTag,
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
    software_contexts: Arc<DeviceSoftwareContexts>,
    pub(super) statistics: BlockIoCounters,
}

impl RuntimeBlockDevice {
    pub(super) fn new(
        parts: LogicalDeviceParts,
        queues: Vec<RuntimeQueue>,
        software_contexts: Arc<DeviceSoftwareContexts>,
    ) -> Self {
        Self {
            id: parts.id,
            name: parts.name,
            info: parts.device_info,
            limits: parts.queue_limits,
            queues: queues.into_boxed_slice(),
            software_contexts,
            statistics: BlockIoCounters::new(),
        }
    }

    pub(in crate::block) fn info(&self) -> DeviceInfo {
        self.info
    }

    pub(in crate::block) fn queue_for_cpu(
        &self,
        cpu: usize,
        operation: RequestOp,
    ) -> Option<&RuntimeQueue> {
        self.software_contexts
            .select(cpu, operation)
            .and_then(|index| self.queues.get(index))
    }

    pub(in crate::block) fn record_successful_io(
        &self,
        operation: rdif_block::RequestOp,
        byte_len: usize,
    ) {
        self.statistics.record_success(operation, byte_len);
    }
}

/// Device-owned immutable software contexts captured from one online-CPU
/// snapshot. Queue selection never advances a cursor and cannot migrate while
/// the device is published.
pub(in crate::block) struct DeviceSoftwareContexts {
    online_cpu_count: usize,
    contexts: [DeviceSoftwareContext; crate::CPU_CAPACITY],
}

impl DeviceSoftwareContexts {
    pub(in crate::block) fn from_queue_info(queues: &[QueueInfo], online_cpu_count: usize) -> Self {
        assert!(
            (1..=crate::CPU_CAPACITY).contains(&online_cpu_count),
            "device software contexts require a valid frozen online CPU count"
        );
        let operation_maps = OperationHctxMaps::from_queue_info(queues);
        let staging_capacity = queues
            .iter()
            .filter(|queue| matches!(queue.kind, rdif_block::QueueKind::Interrupt { .. }))
            .count()
            .saturating_mul(crate::block::hctx::MAX_REQUESTS);
        Self {
            online_cpu_count,
            contexts: core::array::from_fn(|cpu| {
                if cpu < online_cpu_count {
                    DeviceSoftwareContext::from_operation_maps(
                        cpu,
                        &operation_maps,
                        staging_capacity,
                    )
                } else {
                    DeviceSoftwareContext::offline()
                }
            }),
        }
    }

    fn select(&self, cpu: usize, operation: RequestOp) -> Option<usize> {
        if cpu >= self.online_cpu_count {
            return None;
        }
        self.contexts[cpu].hctx_for(operation)
    }

    pub(in crate::block) fn stage(
        &self,
        cpu: usize,
        hctx_index: usize,
        tag: RequestTag,
    ) -> Result<(), HardwareQueueError> {
        let context = self
            .online_context(cpu)
            .ok_or(HardwareQueueError::InvalidCpu(cpu))?;
        context
            .ingress
            .lock()
            .push(DeviceStagedRequest { hctx_index, tag })
    }

    pub(in crate::block) fn remove(&self, hctx_index: usize, tag: RequestTag) -> usize {
        let target = DeviceStagedRequest { hctx_index, tag };
        self.online_contexts()
            .filter(|context| context.ingress.lock().remove(target))
            .count()
    }

    pub(in crate::block) fn readiness_for(&self, hctx_index: usize) -> [bool; crate::CPU_CAPACITY] {
        core::array::from_fn(|cpu| {
            self.online_context(cpu)
                .is_some_and(|context| context.ingress.lock().has_hctx(hctx_index))
        })
    }

    pub(in crate::block) fn pop_for(&self, cpu: usize, hctx_index: usize) -> Option<RequestTag> {
        self.online_context(cpu)?
            .ingress
            .lock()
            .pop_for_hctx(hctx_index)
            .map(|entry| entry.tag)
    }

    pub(in crate::block) fn has_staged_for(&self, hctx_index: usize) -> bool {
        self.online_contexts()
            .any(|context| context.ingress.lock().has_hctx(hctx_index))
    }

    pub(in crate::block) fn clear_hctx(&self, hctx_index: usize) {
        for context in self.online_contexts() {
            context.ingress.lock().clear_hctx(hctx_index);
        }
    }

    fn online_context(&self, cpu: usize) -> Option<&DeviceSoftwareContext> {
        (cpu < self.online_cpu_count).then(|| &self.contexts[cpu])
    }

    fn online_contexts(&self) -> impl Iterator<Item = &DeviceSoftwareContext> {
        self.contexts[..self.online_cpu_count].iter()
    }
}

struct DeviceSoftwareContext {
    read_hctx: Option<usize>,
    write_hctx: Option<usize>,
    flush_hctx: Option<usize>,
    discard_hctx: Option<usize>,
    write_zeroes_hctx: Option<usize>,
    ingress: SpinNoPreempt<DeviceIngressQueue>,
}

impl DeviceSoftwareContext {
    fn from_operation_maps(cpu: usize, maps: &OperationHctxMaps, staging_capacity: usize) -> Self {
        Self {
            read_hctx: maps.read[cpu],
            write_hctx: maps.write[cpu],
            flush_hctx: maps.flush[cpu],
            discard_hctx: maps.discard[cpu],
            write_zeroes_hctx: maps.write_zeroes[cpu],
            ingress: SpinNoPreempt::new(DeviceIngressQueue::new(staging_capacity)),
        }
    }

    fn offline() -> Self {
        Self {
            read_hctx: None,
            write_hctx: None,
            flush_hctx: None,
            discard_hctx: None,
            write_zeroes_hctx: None,
            ingress: SpinNoPreempt::new(DeviceIngressQueue::new(0)),
        }
    }

    const fn hctx_for(&self, operation: RequestOp) -> Option<usize> {
        match operation {
            RequestOp::Read => self.read_hctx,
            RequestOp::Write => self.write_hctx,
            RequestOp::Flush => self.flush_hctx,
            RequestOp::Discard => self.discard_hctx,
            RequestOp::WriteZeroes => self.write_zeroes_hctx,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeviceStagedRequest {
    hctx_index: usize,
    tag: RequestTag,
}

struct DeviceIngressQueue {
    entries: VecDeque<DeviceStagedRequest>,
    capacity: usize,
}

impl DeviceIngressQueue {
    fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push(&mut self, request: DeviceStagedRequest) -> Result<(), HardwareQueueError> {
        if self.entries.len() == self.capacity || self.entries.contains(&request) {
            return Err(HardwareQueueError::Capacity);
        }
        self.entries.push_back(request);
        Ok(())
    }

    fn remove(&mut self, target: DeviceStagedRequest) -> bool {
        let Some(index) = self.entries.iter().position(|entry| *entry == target) else {
            return false;
        };
        self.entries.remove(index);
        true
    }

    fn has_hctx(&self, hctx_index: usize) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.hctx_index == hctx_index)
    }

    fn pop_for_hctx(&mut self, hctx_index: usize) -> Option<DeviceStagedRequest> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.hctx_index == hctx_index)?;
        self.entries.remove(index)
    }

    fn clear_hctx(&mut self, hctx_index: usize) {
        self.entries.retain(|entry| entry.hctx_index != hctx_index);
    }
}

struct OperationHctxMaps {
    read: [Option<usize>; crate::CPU_CAPACITY],
    write: [Option<usize>; crate::CPU_CAPACITY],
    flush: [Option<usize>; crate::CPU_CAPACITY],
    discard: [Option<usize>; crate::CPU_CAPACITY],
    write_zeroes: [Option<usize>; crate::CPU_CAPACITY],
}

impl OperationHctxMaps {
    fn from_queue_info(queues: &[QueueInfo]) -> Self {
        Self {
            read: operation_map(queues, RequestOp::Read),
            write: operation_map(queues, RequestOp::Write),
            flush: operation_map(queues, RequestOp::Flush),
            discard: operation_map(queues, RequestOp::Discard),
            write_zeroes: operation_map(queues, RequestOp::WriteZeroes),
        }
    }
}

fn operation_map(
    queues: &[QueueInfo],
    operation: RequestOp,
) -> [Option<usize>; crate::CPU_CAPACITY] {
    let eligible = queues
        .iter()
        .enumerate()
        .filter_map(|(index, info)| queue_supports(*info, operation).then_some(index))
        .collect::<Vec<_>>();
    core::array::from_fn(|cpu| (!eligible.is_empty()).then(|| eligible[cpu % eligible.len()]))
}

fn queue_supports(info: QueueInfo, operation: RequestOp) -> bool {
    match operation {
        RequestOp::Read => true,
        RequestOp::Write => !info.device.read_only,
        RequestOp::Flush => info.limits.supports_flush,
        RequestOp::Discard => info.limits.supports_discard,
        RequestOp::WriteZeroes => info.limits.supports_write_zeroes,
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

#[cfg(test)]
mod tests {
    use rdif_block::{
        DeviceInfo, IdList, QueueExecution, QueueInfo, QueueKind, QueueLimits, RequestOp,
    };

    use super::*;

    #[test]
    fn cpu_to_hctx_mapping_is_stable_for_the_device_lifetime() {
        let queues = [queue_info(3), queue_info(7)];
        let online_cpu_count = crate::CPU_CAPACITY.min(2);
        let mapping = DeviceSoftwareContexts::from_queue_info(&queues, online_cpu_count);

        assert_eq!(mapping.select(0, RequestOp::Read), Some(0));
        if online_cpu_count > 1 {
            assert_eq!(mapping.select(1, RequestOp::Read), Some(1));
        }
        assert_eq!(
            mapping.select(0, RequestOp::Read),
            mapping.select(0, RequestOp::Read),
            "queue selection must not depend on a global dispatch cursor"
        );
        assert_eq!(
            mapping.select(online_cpu_count, RequestOp::Read),
            None,
            "a CPU outside the frozen online snapshot has no software context"
        );
    }

    #[test]
    fn one_cpu_ingress_keeps_hctx_identity_without_per_hctx_context_arrays() {
        let queues = [queue_info(3), queue_info(7)];
        let contexts = DeviceSoftwareContexts::from_queue_info(&queues, 1);
        let first = RequestTag::from_request_id(rdif_block::RequestId::new(65)).unwrap();
        let second = RequestTag::from_request_id(rdif_block::RequestId::new(66)).unwrap();

        contexts.stage(0, 0, first).unwrap();
        contexts.stage(0, 1, second).unwrap();

        assert!(contexts.readiness_for(0)[0]);
        assert!(contexts.readiness_for(1)[0]);
        assert_eq!(contexts.pop_for(0, 1), Some(second));
        assert_eq!(contexts.pop_for(0, 0), Some(first));
        assert!(!contexts.has_staged_for(0));
        assert!(!contexts.has_staged_for(1));
    }

    fn queue_info(id: usize) -> QueueInfo {
        let mut sources = IdList::none();
        sources.insert(0);
        QueueInfo {
            id,
            device: DeviceInfo::new(1024, 512),
            limits: QueueLimits::simple(512, u64::MAX),
            kind: QueueKind::Interrupt { sources },
            execution: QueueExecution::Tagged,
        }
    }
}
