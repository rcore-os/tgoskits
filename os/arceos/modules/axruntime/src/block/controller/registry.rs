//! Runtime controller registry, queue construction, and normal IRQ route binding.

use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use ax_driver::{BindingLocator, block::RdifBlockDevice};
use ax_kspin::SpinNoPreempt;
use rdif_block::{
    BundleError, ControllerBundle, IdList, IrqSourceInfo, LogicalDevice, LogicalDeviceParts,
    QueueHandle, QueueKind, validate_controller_devices, validate_queue_activation,
};

use super::{
    BlockController, BlockControllerError, ControllerOwnerLink, InlineQueue, MAX_HARDWARE_QUEUES,
    RuntimeBlockDevice, RuntimeIrqSource, RuntimeQueue, device::RejectedOwnerQuarantine,
};
use crate::{
    block::{HardwareQueue, HostPciEndpoint, HostPhysicalRange, statistics::BlockIoStats},
    irq::Registration,
};

pub(super) fn capture_host_physical_ranges(
    device: &RdifBlockDevice,
) -> Result<Box<[HostPhysicalRange]>, BlockControllerError> {
    device
        .host_mmio_ranges()
        .iter()
        .map(|range| {
            HostPhysicalRange::new(range.base(), range.length()).map_err(|source| {
                BlockControllerError::InvalidHostResource {
                    controller: String::from(device.name()),
                    source,
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
}

pub(super) fn capture_pci_endpoint(device: &RdifBlockDevice) -> Option<HostPciEndpoint> {
    match device.locator() {
        BindingLocator::Pci {
            segment,
            bus,
            device,
            function,
        } => Some(HostPciEndpoint::new(*segment, *bus, *device, *function)),
        _ => None,
    }
}

/// Takes every driver-core block controller and publishes only those whose
/// complete IRQ-only activation succeeds.
pub fn activate_discovered_controllers() -> Vec<Arc<BlockController>> {
    let mut activated = Vec::new();
    for device in ax_driver::block::take_rdif_block_devices() {
        match BlockController::activate(device) {
            Ok(controller) => {
                info!("activated IRQ-only block controller {}", controller.name());
                RUNTIME_CONTROLLERS.lock().push(Arc::clone(&controller));
                activated.push(controller);
            }
            Err(error) => error!("block controller activation failed closed: {error}"),
        }
    }
    activated
}

pub(in crate::block) fn runtime_handoff_controllers() -> Vec<(usize, Arc<BlockController>)> {
    RUNTIME_CONTROLLERS
        .lock()
        .iter()
        .enumerate()
        .filter(|(_, controller)| controller.has_interrupt_queues())
        .map(|(slot, controller)| (slot, Arc::clone(controller)))
        .collect()
}

static RUNTIME_CONTROLLERS: SpinNoPreempt<Vec<Arc<BlockController>>> =
    SpinNoPreempt::new(Vec::new());

/// Aggregates successful I/O completed by all runtime-owned controllers.
pub fn block_io_stats() -> BlockIoStats {
    RUNTIME_CONTROLLERS
        .lock()
        .iter()
        .fold(BlockIoStats::default(), |total, controller| {
            total.saturating_add(controller.io_stats())
        })
}

pub(super) fn materialize_logical_devices(
    device: &mut RdifBlockDevice,
) -> Result<Vec<LogicalDevice>, BlockControllerError> {
    let cpu_queue_limit = crate::runtime_cpu_count().clamp(1, MAX_HARDWARE_QUEUES);
    materialize_bundle_logical_devices(device.bundle_mut(), cpu_queue_limit)
}

fn materialize_bundle_logical_devices(
    bundle: &mut dyn ControllerBundle,
    cpu_queue_limit: usize,
) -> Result<Vec<LogicalDevice>, BlockControllerError> {
    let mut logical_devices = Vec::new();
    if let Err(error) =
        try_materialize_bundle_logical_devices(bundle, cpu_queue_limit, &mut logical_devices)
    {
        shutdown_logical_device_iter(logical_devices.into_iter());
        return Err(error);
    }
    Ok(logical_devices)
}

fn try_materialize_bundle_logical_devices(
    bundle: &mut dyn ControllerBundle,
    cpu_queue_limit: usize,
    logical_devices: &mut Vec<LogicalDevice>,
) -> Result<(), BlockControllerError> {
    let mut expected_ids = bundle.logical_device_ids();
    let device_ids = expected_ids.iter().collect::<Vec<_>>();
    if device_ids.is_empty() {
        validate_controller_devices(&[])?;
    }

    let mut remaining_queue_capacity = MAX_HARDWARE_QUEUES;
    logical_devices.reserve(device_ids.len());
    for (index, device_id) in device_ids.iter().copied().enumerate() {
        let remaining_devices = device_ids.len() - index;
        let fair_capacity = remaining_queue_capacity / remaining_devices;
        let queue_budget = cpu_queue_limit.min(fair_capacity).max(1);
        let queue_budget = NonZeroUsize::new(queue_budget)
            .expect("a remaining logical device always retains one queue slot");
        let logical_device = bundle.take_logical_device(device_id, queue_budget)?;
        logical_devices.push(logical_device);
        let logical_device = logical_devices
            .last()
            .expect("the extracted logical device was just published for rollback");
        if logical_device.id() != device_id {
            return Err(BundleError::UnexpectedDeviceId {
                requested: device_id,
                returned: logical_device.id(),
            }
            .into());
        }
        if logical_device.queue_count() > queue_budget.get() {
            return Err(BundleError::QueueLimitExceeded {
                device_id,
                max_queues: queue_budget.get(),
            }
            .into());
        }
        remaining_queue_capacity -= logical_device.queue_count();

        expected_ids.remove(device_id);
        let actual_ids = bundle.logical_device_ids();
        if actual_ids != expected_ids {
            return Err(BundleError::DeviceSetChanged {
                expected_bits: expected_ids.bits(),
                actual_bits: actual_ids.bits(),
            }
            .into());
        }
    }

    validate_controller_devices(logical_devices)?;
    Ok(())
}

pub(super) fn create_runtime_devices(
    logical_devices: Vec<LogicalDevice>,
    owner_link: &'static ControllerOwnerLink,
    lifecycle_cookie: usize,
) -> Result<Vec<RuntimeBlockDevice>, BlockControllerError> {
    validate_controller_devices(&logical_devices)?;
    let mut pending_devices = logical_devices.into_iter();
    let mut runtime_devices = Vec::new();
    let mut affinity_slot = 0;

    while let Some(logical_device) = pending_devices.next() {
        let LogicalDeviceParts {
            id,
            name,
            device_info,
            queue_limits,
            queues,
        } = logical_device.into_parts();
        let mut pending_queues = queues.into_iter();
        let mut runtime_queues = Vec::new();
        while let Some(mut queue) = pending_queues.next() {
            let info = queue.info();
            let runtime_queue = match info.kind {
                QueueKind::Inline => RuntimeQueue::Inline(Box::new(InlineQueue::new(queue))),
                QueueKind::Interrupt { .. } => {
                    if let Err(error) = queue.bind_interrupt_controller(
                        lifecycle_cookie,
                        rdif_block::ControllerEpoch::INITIAL,
                    ) {
                        shutdown_unpublished_queue(queue);
                        rollback_unpublished_runtime_queues(&runtime_queues);
                        shutdown_queue_iter(pending_queues);
                        shutdown_logical_device_iter(pending_devices);
                        rollback_unpublished_runtime_devices(&runtime_devices);
                        return Err(error.into());
                    }
                    let cpu = affinity_slot % crate::runtime_cpu_count().max(1);
                    match HardwareQueue::activate(queue, cpu, owner_link, lifecycle_cookie) {
                        Ok(queue) => RuntimeQueue::Interrupt(queue),
                        Err(error) => {
                            rollback_unpublished_runtime_queues(&runtime_queues);
                            shutdown_queue_iter(pending_queues);
                            shutdown_logical_device_iter(pending_devices);
                            rollback_unpublished_runtime_devices(&runtime_devices);
                            return Err(error.into());
                        }
                    }
                }
            };
            affinity_slot += 1;
            runtime_queues.push(runtime_queue);
        }
        runtime_devices.push(RuntimeBlockDevice::new(
            LogicalDeviceParts {
                id,
                name,
                device_info,
                queue_limits,
                queues: Vec::new(),
            },
            runtime_queues,
        ));
    }
    Ok(runtime_devices)
}

pub(super) fn rollback_unpublished_runtime_devices(devices: &[RuntimeBlockDevice]) {
    for queue in devices.iter().flat_map(|device| device.queues.iter()) {
        rollback_unpublished_runtime_queue(queue);
    }
}

fn rollback_unpublished_runtime_queues(queues: &[RuntimeQueue]) {
    for queue in queues {
        rollback_unpublished_runtime_queue(queue);
    }
}

fn rollback_unpublished_runtime_queue(queue: &RuntimeQueue) {
    match queue {
        RuntimeQueue::Inline(queue) => {
            let _ = queue.shutdown_unpublished();
        }
        RuntimeQueue::Interrupt(queue) => queue.abort_unpublished_after_irq_quiesce(),
    }
}

fn shutdown_queue_iter(queues: impl Iterator<Item = QueueHandle>) {
    for queue in queues {
        shutdown_unpublished_queue(queue);
    }
}

pub(super) fn shutdown_logical_device_iter(devices: impl Iterator<Item = LogicalDevice>) {
    for device in devices {
        shutdown_queue_iter(device.into_parts().queues.into_iter());
    }
}

pub(super) fn register_irq_routes_disabled(
    controller_name: &str,
    device: &mut RdifBlockDevice,
    devices: &[RuntimeBlockDevice],
    declared_sources: &[IrqSourceInfo],
    owner_link: &'static ControllerOwnerLink,
) -> Result<
    (
        Vec<Registration>,
        Vec<&'static RuntimeIrqSource>,
        IdList,
    ),
    BlockControllerError,
> {
    let mut registrations = Vec::new();
    let mut runtime_sources = Vec::new();
    let mut bound_sources = IdList::none();

    for source in declared_sources {
        let routes = devices
            .iter()
            .flat_map(|device| device.queues.iter())
            .filter(|queue| source.queues.contains(queue.info().id))
            .filter_map(RuntimeQueue::interrupt_queue)
            .collect::<Vec<_>>();
        if routes.is_empty() {
            continue;
        }
        let binding = device
            .irq_for_source(source.id)
            .cloned()
            .ok_or(BlockControllerError::MissingIrqBinding(source.id))?;
        let irq = crate::irq::resolve_binding_irq(binding)?;
        let handler = device
            .bundle_mut()
            .take_irq_handler(source.id)
            .ok_or(BlockControllerError::MissingIrqHandler(source.id))?;
        let source_id = source.id;
        let action_name = format!("{controller_name}/blk-source-{source_id}");
        let affinity_cpu = routes[0].affinity_cpu();
        let source = RuntimeIrqSource::allocate(source_id, routes, handler, owner_link);
        let registration = Registration::register_shared_disabled_on(
            action_name,
            irq,
            affinity_cpu,
            move |_ctx| source.handle_irq(),
        )?;
        bound_sources.insert(source_id);
        registrations.push(registration);
        runtime_sources.push(source);
    }
    Ok((registrations, runtime_sources, bound_sources))
}

pub(super) fn validate_runtime_devices(
    devices: &[RuntimeBlockDevice],
    declared_sources: &[IrqSourceInfo],
    bound_sources: IdList,
) -> Result<(), BlockControllerError> {
    for queue in devices.iter().flat_map(|device| device.queues.iter()) {
        validate_queue_activation(queue.info(), declared_sources, bound_sources)?;
    }
    Ok(())
}

fn shutdown_unpublished_queue(mut queue: QueueHandle) {
    let mut sink = RejectedOwnerQuarantine::new();
    let _ = queue.shutdown(&mut sink);
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use core::{
        any::Any,
        num::NonZeroUsize,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use rdif_block::{
        BIrqHandler, BlkError, BundleError, CompletedRequest, CompletionSink, ControllerBundle,
        ControllerInitEndpoint, DeviceInfo, DispatchMode, DriverGeneric, IQueue, IrqSourceList,
        LifecycleEndpoint, LogicalDevice, LogicalDeviceId, LogicalDeviceIds, OwnedRequest,
        QueueEventBatch, QueueHandle, QueueInfo, QueueKind, QueueLimits, RequestId,
        ServiceProgress, SubmitError, SubmitOutcome,
    };

    use super::{BlockControllerError, materialize_bundle_logical_devices};

    #[test]
    fn second_device_failure_explicitly_shuts_down_the_first_device_queues() {
        let shutdowns = Box::leak(Box::new(AtomicUsize::new(0)));
        let mut bundle = FailingSecondDeviceBundle::new(shutdowns);

        let error = materialize_bundle_logical_devices(&mut bundle, 1).unwrap_err();

        assert!(matches!(
            error,
            BlockControllerError::Bundle(BundleError::DeviceUnavailable { device_id })
                if device_id == LogicalDeviceId::new(1).unwrap()
        ));
        assert_eq!(shutdowns.load(Ordering::Acquire), 1);
    }

    struct FailingSecondDeviceBundle {
        remaining: LogicalDeviceIds,
        shutdowns: &'static AtomicUsize,
    }

    impl FailingSecondDeviceBundle {
        fn new(shutdowns: &'static AtomicUsize) -> Self {
            Self {
                remaining: LogicalDeviceIds::from_bits(0b11),
                shutdowns,
            }
        }
    }

    impl DriverGeneric for FailingSecondDeviceBundle {
        fn name(&self) -> &str {
            "failing-multi-device-controller"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
        }
    }

    impl ControllerBundle for FailingSecondDeviceBundle {
        fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
            ControllerInitEndpoint::Ready
        }

        fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
            LifecycleEndpoint::Inline
        }

        fn logical_device_ids(&self) -> LogicalDeviceIds {
            self.remaining
        }

        fn take_logical_device(
            &mut self,
            device_id: LogicalDeviceId,
            _max_queues: NonZeroUsize,
        ) -> Result<LogicalDevice, BundleError> {
            if device_id != LogicalDeviceId::new(0).unwrap() {
                return Err(BundleError::DeviceUnavailable { device_id });
            }
            self.remaining.remove(device_id);
            let device = DeviceInfo::new(128, 512);
            let limits = QueueLimits::simple(512, u64::MAX);
            let queue = QueueHandle::new(Box::new(ShutdownTrackingQueue {
                info: QueueInfo {
                    id: 0,
                    device,
                    limits,
                    kind: QueueKind::Inline,
                    dispatch_mode: DispatchMode::Direct,
                },
                shutdowns: self.shutdowns,
            }));
            Ok(LogicalDevice::new(
                device_id,
                "first-disk".into(),
                device,
                limits,
                vec![queue],
            ))
        }

        fn enable_irq(&self) -> Result<(), BlkError> {
            Ok(())
        }

        fn disable_irq(&self) -> Result<(), BlkError> {
            Ok(())
        }

        fn is_irq_enabled(&self) -> bool {
            false
        }

        fn irq_sources(&self) -> IrqSourceList {
            Vec::new()
        }

        fn take_irq_handler(&mut self, _source_id: usize) -> Option<BIrqHandler> {
            None
        }
    }

    struct ShutdownTrackingQueue {
        info: QueueInfo,
        shutdowns: &'static AtomicUsize,
    }

    impl IQueue for ShutdownTrackingQueue {
        fn id(&self) -> usize {
            self.info.id
        }

        fn info(&self) -> QueueInfo {
            self.info
        }

        fn submit_owned(
            &mut self,
            id: RequestId,
            request: OwnedRequest,
        ) -> Result<SubmitOutcome, SubmitError> {
            Ok(SubmitOutcome::Completed(CompletedRequest::new(
                id,
                Ok(()),
                request,
            )))
        }

        fn service_events(
            &mut self,
            _events: &QueueEventBatch<'_>,
            _sink: &mut dyn CompletionSink,
        ) -> Result<ServiceProgress, BlkError> {
            Err(BlkError::NotSupported)
        }

        fn reclaim_after_quiesce(
            &mut self,
            _proof: &rdif_block::DmaQuiesced,
            _sink: &mut dyn CompletionSink,
        ) -> Result<(), BlkError> {
            Err(BlkError::NotSupported)
        }

        fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            self.shutdowns.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }
}
