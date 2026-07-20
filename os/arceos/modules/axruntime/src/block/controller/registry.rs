//! Runtime controller registry, queue construction, and normal IRQ route binding.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use ax_driver::{BindingLocator, block::RdifBlockDevice};
use ax_kspin::SpinNoPreempt;
use rdif_block::{
    BundleError, ControllerBundle, IdList, IrqSourceInfo, LogicalDevice, LogicalDeviceParts,
    QueueHandle, QueueKind, UnpublishedQueueQuarantine, validate_controller_devices,
    validate_queue_activation,
};

use super::{
    BlockController, BlockControllerError, ControllerOwnerLink, InlineQueue, MAX_HARDWARE_QUEUES,
    RuntimeBlockDevice, RuntimeQueue,
};
use crate::{
    block::{
        HardwareQueue, HostPciEndpoint, HostPhysicalRange, quarantine::QueueQuarantineReservations,
        statistics::BlockIoStats,
    },
    maintenance::DeviceMaintenanceHandle,
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
    quarantine_reservations: &mut QueueQuarantineReservations,
) -> Result<Vec<LogicalDevice>, BlockControllerError> {
    let cpu_queue_limit = crate::runtime_cpu_count().clamp(1, MAX_HARDWARE_QUEUES);
    materialize_bundle_logical_devices(
        device.bundle_mut(),
        cpu_queue_limit,
        quarantine_reservations,
    )
}

fn materialize_bundle_logical_devices(
    bundle: &mut dyn ControllerBundle,
    cpu_queue_limit: usize,
    quarantine_reservations: &mut QueueQuarantineReservations,
) -> Result<Vec<LogicalDevice>, BlockControllerError> {
    let mut logical_devices = Vec::new();
    if let Err(error) = try_materialize_bundle_logical_devices(
        bundle,
        cpu_queue_limit,
        &mut logical_devices,
        quarantine_reservations,
    ) {
        shutdown_logical_device_iter(logical_devices.into_iter(), quarantine_reservations);
        return Err(error);
    }
    Ok(logical_devices)
}

fn try_materialize_bundle_logical_devices(
    bundle: &mut dyn ControllerBundle,
    cpu_queue_limit: usize,
    logical_devices: &mut Vec<LogicalDevice>,
    quarantine_reservations: &mut QueueQuarantineReservations,
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
        let logical_device = match bundle.take_logical_device(device_id, queue_budget) {
            Ok(device) => device,
            Err(BundleError::UnpublishedQueuesQuarantined(quarantine)) => {
                return Err(retain_unpublished_quarantine(
                    quarantine,
                    quarantine_reservations,
                ));
            }
            Err(error) => return Err(error.into()),
        };
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

fn retain_unpublished_quarantine(
    quarantine: UnpublishedQueueQuarantine,
    reservations: &mut QueueQuarantineReservations,
) -> BlockControllerError {
    let error = BlockControllerError::UnpublishedQueuesQuarantined {
        device_id: quarantine.device_id(),
        device_name: String::from(quarantine.device_name()),
        contract: quarantine.contract_error(),
        close_error: quarantine.reason(),
        queue_count: quarantine.queue_count(),
    };
    for queue in quarantine.into_queues() {
        let reservation = reservations.bind(queue.info());
        crate::block::quarantine::retain_unpublished_quarantine(queue, reservation);
    }
    error
}

pub(super) fn create_runtime_devices(
    logical_devices: Vec<LogicalDevice>,
    maintenance: Arc<DeviceMaintenanceHandle<super::source::BlockMaintenanceEvent>>,
    owner_link: Arc<ControllerOwnerLink>,
    lifecycle_cookie: usize,
    quarantine_reservations: &mut QueueQuarantineReservations,
) -> Result<Vec<RuntimeBlockDevice>, BlockControllerError> {
    validate_controller_devices(&logical_devices)?;
    let mut pending_devices = logical_devices.into_iter();
    let mut runtime_devices = Vec::new();
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
            let quarantine_reservation = quarantine_reservations.bind(info);
            let runtime_queue = match info.kind {
                QueueKind::Inline => {
                    RuntimeQueue::Inline(Box::new(InlineQueue::new(queue, quarantine_reservation)))
                }
                QueueKind::Interrupt { .. } => {
                    if let Err(error) = queue.bind_interrupt_controller(
                        lifecycle_cookie,
                        rdif_block::ControllerEpoch::INITIAL,
                    ) {
                        shutdown_unpublished_queue(queue, quarantine_reservation);
                        rollback_unpublished_runtime_queues(&runtime_queues);
                        shutdown_queue_iter(pending_queues, quarantine_reservations);
                        shutdown_logical_device_iter(pending_devices, quarantine_reservations);
                        rollback_unpublished_runtime_devices(&runtime_devices);
                        return Err(error.into());
                    }
                    match HardwareQueue::activate(
                        queue,
                        quarantine_reservation,
                        Arc::clone(&maintenance),
                        Arc::clone(&owner_link),
                        lifecycle_cookie,
                    ) {
                        Ok(queue) => RuntimeQueue::Interrupt(queue),
                        Err(error) => {
                            rollback_unpublished_runtime_queues(&runtime_queues);
                            shutdown_queue_iter(pending_queues, quarantine_reservations);
                            shutdown_logical_device_iter(pending_devices, quarantine_reservations);
                            rollback_unpublished_runtime_devices(&runtime_devices);
                            return Err(error.into());
                        }
                    }
                }
            };
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

fn shutdown_queue_iter(
    queues: impl Iterator<Item = QueueHandle>,
    quarantine_reservations: &mut QueueQuarantineReservations,
) {
    for queue in queues {
        let reservation = quarantine_reservations.bind(queue.info());
        shutdown_unpublished_queue(queue, reservation);
    }
}

pub(super) fn shutdown_logical_device_iter(
    devices: impl Iterator<Item = LogicalDevice>,
    quarantine_reservations: &mut QueueQuarantineReservations,
) {
    for device in devices {
        shutdown_queue_iter(
            device.into_parts().queues.into_iter(),
            quarantine_reservations,
        );
    }
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

fn shutdown_unpublished_queue(
    queue: QueueHandle,
    reservation: crate::block::quarantine::QueueQuarantineReservation,
) {
    let _ = crate::block::quarantine::close_or_quarantine(queue, reservation);
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
        BlkError, BundleError, CompletedRequest, CompletionSink, ControllerBundle,
        ControllerInitEndpoint, DeviceInfo, DriverGeneric, IQueue, IdList, IrqSourceInfo,
        IrqSourceList, LifecycleEndpoint, LogicalDevice, LogicalDeviceId, LogicalDeviceIds,
        OwnedRequest, QueueEventBatch, QueueExecution, QueueHandle, QueueInfo, QueueKind,
        QueueLimits, RequestId, ServiceProgress, SubmitError, SubmitOutcome,
    };

    use super::{BlockControllerError, materialize_bundle_logical_devices};
    use crate::block::quarantine::QueueQuarantineReservations;

    #[test]
    fn second_device_failure_explicitly_shuts_down_the_first_device_queues() {
        static SHUTDOWNS: AtomicUsize = AtomicUsize::new(0);
        SHUTDOWNS.store(0, Ordering::Release);
        let mut bundle = FailingSecondDeviceBundle::new(&SHUTDOWNS);
        let mut quarantine_reservations = QueueQuarantineReservations::reserve(64).unwrap();

        let error =
            materialize_bundle_logical_devices(&mut bundle, 1, &mut quarantine_reservations)
                .unwrap_err();

        assert!(matches!(
            error,
            BlockControllerError::Bundle(BundleError::DeviceUnavailable { device_id })
                if device_id == LogicalDeviceId::new(1).unwrap()
        ));
        assert_eq!(SHUTDOWNS.load(Ordering::Acquire), 1);
    }

    #[test]
    fn normal_irq_topology_is_frozen_after_queue_materialization() {
        static SHUTDOWNS: AtomicUsize = AtomicUsize::new(0);
        SHUTDOWNS.store(0, Ordering::Release);
        let mut bundle = QueueDefinedIrqBundle {
            remaining: LogicalDeviceIds::from_bits(1),
            queue_materialized: false,
            shutdowns: &SHUTDOWNS,
        };
        let mut quarantine_reservations = QueueQuarantineReservations::reserve(64).unwrap();

        assert!(bundle.irq_sources().is_empty());
        let devices =
            materialize_bundle_logical_devices(&mut bundle, 1, &mut quarantine_reservations)
                .unwrap();
        let declared = bundle.irq_sources();

        assert_eq!(declared, vec![IrqSourceInfo::legacy(IdList::from_bits(1))]);
        assert_eq!(
            devices[0]
                .queues()
                .next()
                .expect("the materialized disk must own one queue")
                .info()
                .kind,
            QueueKind::Interrupt {
                sources: IdList::from_bits(1),
            }
        );
        super::shutdown_logical_device_iter(devices.into_iter(), &mut quarantine_reservations);
    }

    struct FailingSecondDeviceBundle {
        remaining: LogicalDeviceIds,
        shutdowns: &'static AtomicUsize,
    }

    struct QueueDefinedIrqBundle {
        remaining: LogicalDeviceIds,
        queue_materialized: bool,
        shutdowns: &'static AtomicUsize,
    }

    impl DriverGeneric for QueueDefinedIrqBundle {
        fn name(&self) -> &str {
            "queue-defined-irq-controller"
        }
    }

    impl ControllerBundle for QueueDefinedIrqBundle {
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
            if !self.remaining.contains(device_id) {
                return Err(BundleError::DeviceUnavailable { device_id });
            }
            self.remaining.remove(device_id);
            self.queue_materialized = true;
            let device = DeviceInfo::new(128, 512);
            let limits = QueueLimits::simple(512, u64::MAX);
            let queue = QueueHandle::new(Box::new(ShutdownTrackingQueue {
                info: QueueInfo {
                    id: 0,
                    device,
                    limits,
                    kind: QueueKind::Interrupt {
                        sources: IdList::from_bits(1),
                    },
                    execution: QueueExecution::Tagged,
                },
                shutdowns: self.shutdowns,
            }));
            Ok(LogicalDevice::new(
                device_id,
                "queue-defined-disk".into(),
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
            self.queue_materialized
                .then(|| vec![IrqSourceInfo::legacy(IdList::from_bits(1))])
                .unwrap_or_default()
        }

        fn take_irq_source(&mut self, _source_id: usize) -> Option<rdif_block::BlockIrqSource> {
            None
        }
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
                    execution: QueueExecution::Inline,
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

        fn take_irq_source(&mut self, _source_id: usize) -> Option<rdif_block::BlockIrqSource> {
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

        fn shutdown(&mut self) -> Result<(), BlkError> {
            self.shutdowns.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }
}
