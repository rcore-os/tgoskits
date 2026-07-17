use core::num::NonZeroUsize;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use rdif_block::{
    BlkError, CompletedRequest, CompletionSink, ControllerBundle, ControllerInitEndpoint,
    DeviceInfo, DispatchMode, DriverGeneric, IQueue, LifecycleEndpoint, LogicalDeviceId,
    OwnedRequest, QueueContractError, QueueEventBatch, QueueHandle, QueueInfo, QueueKind,
    QueueLimits, RequestId, ServiceProgress, SingleDeviceBundle, SubmitError, SubmitOutcome,
    validate_controller_devices,
};

struct InlineQueue {
    info: QueueInfo,
}

impl IQueue for InlineQueue {
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
        Ok(())
    }
}

struct MismatchedIdentityQueue {
    advertised_id: usize,
    info: QueueInfo,
}

impl IQueue for MismatchedIdentityQueue {
    fn id(&self) -> usize {
        self.advertised_id
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
        Ok(())
    }
}

struct LegacySingleDevice {
    queue: Option<QueueHandle>,
}

impl LegacySingleDevice {
    fn new() -> Self {
        let device = DeviceInfo::new(64, 512);
        let limits = QueueLimits::simple(512, u64::MAX);
        let info = QueueInfo {
            id: 7,
            device,
            limits,
            kind: QueueKind::Inline,
            dispatch_mode: DispatchMode::Direct,
        };
        Self {
            queue: Some(QueueHandle::new(Box::new(InlineQueue { info }))),
        }
    }
}

impl DriverGeneric for LegacySingleDevice {
    fn name(&self) -> &str {
        "legacy-inline"
    }
}

impl rdif_block::Interface for LegacySingleDevice {
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
        ControllerInitEndpoint::Ready
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Inline
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo::new(64, 512)
    }

    fn queue_limits(&self) -> QueueLimits {
        QueueLimits::simple(512, u64::MAX)
    }

    fn create_queue(&mut self) -> Option<QueueHandle> {
        self.queue.take()
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

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        Vec::new()
    }

    fn take_irq_handler(&mut self, _source_id: usize) -> Option<rdif_block::BIrqHandler> {
        None
    }
}

#[derive(Default)]
struct ShutdownSink;

impl CompletionSink for ShutdownSink {
    fn complete(&mut self, completion: CompletedRequest) {
        panic!("unpublished inline queue returned unexpected completion {completion:?}");
    }
}

#[test]
fn legacy_interface_is_an_explicit_single_device_bundle() {
    let mut bundle = SingleDeviceBundle::new(Box::new(LegacySingleDevice::new()));
    let device_id = LogicalDeviceId::new(0).unwrap();

    assert_eq!(
        bundle.logical_device_ids().iter().collect::<Vec<_>>(),
        [device_id]
    );
    let device = bundle
        .take_logical_device(device_id, NonZeroUsize::new(4).unwrap())
        .unwrap();
    assert_eq!(device.id(), device_id);
    assert_eq!(device.name(), "legacy-inline");
    assert_eq!(device.device_info().num_blocks, 64);
    assert_eq!(device.queue_count(), 1);
    assert!(bundle.logical_device_ids().is_empty());

    let mut parts = device.into_parts();
    assert_eq!(parts.queues[0].id(), 7);
    parts.queues[0]
        .shutdown(&mut ShutdownSink)
        .expect("an unpublished inline queue must shut down");
}

#[test]
fn a_logical_device_cannot_be_extracted_twice() {
    let mut bundle = SingleDeviceBundle::new(Box::new(LegacySingleDevice::new()));
    let device_id = LogicalDeviceId::new(0).unwrap();
    let device = bundle
        .take_logical_device(device_id, NonZeroUsize::MIN)
        .unwrap();

    let error = bundle
        .take_logical_device(device_id, NonZeroUsize::MIN)
        .unwrap_err();
    assert!(matches!(
        error,
        rdif_block::BundleError::DeviceUnavailable { device_id: rejected }
            if rejected == device_id
    ));

    let mut parts = device.into_parts();
    parts.queues[0].shutdown(&mut ShutdownSink).unwrap();
}

#[test]
fn single_device_bundle_remains_available_after_empty_queue_attempt() {
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let interface = ScriptedLegacyDevice::empty_then_valid(Arc::clone(&shutdowns));
    let mut bundle = SingleDeviceBundle::new(Box::new(interface));
    let device_id = LogicalDeviceId::new(0).unwrap();

    let error = bundle
        .take_logical_device(device_id, NonZeroUsize::MIN)
        .unwrap_err();

    assert!(matches!(
        error,
        rdif_block::BundleError::NoQueues { device_id: rejected } if rejected == device_id
    ));
    assert!(bundle.logical_device_ids().contains(device_id));

    let device = bundle
        .take_logical_device(device_id, NonZeroUsize::MIN)
        .expect("a failed empty extraction must remain retryable");
    shutdown_devices(vec![device]);
    assert_eq!(shutdowns.load(Ordering::Acquire), 1);
}

#[test]
fn single_device_bundle_shuts_down_invalid_extracted_queue_before_retry() {
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let interface = ScriptedLegacyDevice::invalid_then_valid(Arc::clone(&shutdowns));
    let mut bundle = SingleDeviceBundle::new(Box::new(interface));
    let device_id = LogicalDeviceId::new(0).unwrap();

    assert!(
        bundle
            .take_logical_device(device_id, NonZeroUsize::MIN)
            .is_err()
    );
    assert_eq!(shutdowns.load(Ordering::Acquire), 1);
    assert!(bundle.logical_device_ids().contains(device_id));

    let device = bundle
        .take_logical_device(device_id, NonZeroUsize::MIN)
        .expect("rollback must preserve a retryable single-device adapter");
    shutdown_devices(vec![device]);
    assert_eq!(shutdowns.load(Ordering::Acquire), 2);
}

#[test]
fn failed_rollback_quarantines_the_single_device_bundle() {
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let interface = ScriptedLegacyDevice::invalid_with_failed_shutdown(Arc::clone(&shutdowns));
    let mut bundle = SingleDeviceBundle::new(Box::new(interface));
    let device_id = LogicalDeviceId::new(0).unwrap();

    assert!(matches!(
        bundle.take_logical_device(device_id, NonZeroUsize::MIN),
        Err(rdif_block::BundleError::Driver(BlkError::Io))
    ));
    assert_eq!(shutdowns.load(Ordering::Acquire), 1);
    assert!(
        bundle.logical_device_ids().is_empty(),
        "a failed unpublished-queue shutdown must remain unavailable"
    );
    assert!(matches!(
        bundle.take_logical_device(device_id, NonZeroUsize::MIN),
        Err(rdif_block::BundleError::DeviceUnavailable { .. })
    ));
}

#[test]
fn controller_queue_ids_are_global_but_device_geometry_remains_independent() {
    let devices = vec![
        logical_device(0, 4_096, 3, 4_096),
        logical_device(1, 16_384, 11, 16_384),
    ];

    assert_eq!(validate_controller_devices(&devices), Ok(()));
    shutdown_devices(devices);
}

#[test]
fn duplicate_queue_identity_across_devices_is_rejected() {
    let devices = vec![
        logical_device(0, 4_096, 3, 4_096),
        logical_device(1, 16_384, 3, 16_384),
    ];

    assert_eq!(
        validate_controller_devices(&devices),
        Err(QueueContractError::DuplicateControllerQueueId { queue_id: 3 })
    );
    shutdown_devices(devices);
}

#[test]
fn queue_metadata_cannot_redirect_dispatch_to_a_sibling_address_space() {
    let devices = vec![logical_device(1, 16_384, 11, 4_096)];

    assert_eq!(
        validate_controller_devices(&devices),
        Err(QueueContractError::QueueDeviceMetadataMismatch {
            device_id: 1,
            queue_id: 11,
        })
    );
    shutdown_devices(devices);
}

#[test]
fn controller_validation_rejects_an_interrupt_queue_without_irq_sources() {
    let device = DeviceInfo::new(64, 512);
    let limits = QueueLimits::simple(512, u64::MAX);
    let queue_info = QueueInfo {
        id: 4,
        device,
        limits,
        kind: QueueKind::Interrupt {
            sources: rdif_block::IdList::none(),
        },
        dispatch_mode: DispatchMode::Direct,
    };
    let devices = vec![rdif_block::LogicalDevice::new(
        LogicalDeviceId::new(0).unwrap(),
        "invalid-interrupt".into(),
        device,
        limits,
        vec![QueueHandle::new(Box::new(InlineQueue { info: queue_info }))],
    )];

    assert_eq!(
        validate_controller_devices(&devices),
        Err(QueueContractError::MissingInterruptSources { queue_id: 4 })
    );
    shutdown_devices(devices);
}

#[test]
fn controller_validation_rejects_conflicting_queue_identity_sources() {
    let device = DeviceInfo::new(64, 512);
    let limits = QueueLimits::simple(512, u64::MAX);
    let queue_info = QueueInfo {
        id: 4,
        device,
        limits,
        kind: QueueKind::Inline,
        dispatch_mode: DispatchMode::Direct,
    };
    let devices = vec![rdif_block::LogicalDevice::new(
        LogicalDeviceId::new(0).unwrap(),
        "identity-mismatch".into(),
        device,
        limits,
        vec![QueueHandle::new(Box::new(MismatchedIdentityQueue {
            advertised_id: 9,
            info: queue_info,
        }))],
    )];

    assert_eq!(
        validate_controller_devices(&devices),
        Err(QueueContractError::QueueIdentityMismatch {
            advertised_id: 9,
            metadata_id: 4,
        })
    );
    shutdown_devices(devices);
}

#[test]
fn single_device_bundle_rejects_a_queue_budget_above_the_controller_id_space() {
    let mut bundle = SingleDeviceBundle::new(Box::new(LegacySingleDevice::new()));
    let device_id = LogicalDeviceId::new(0).unwrap();
    let oversized_budget = NonZeroUsize::new(rdif_block::MAX_CONTROLLER_QUEUES + 1).unwrap();

    assert!(matches!(
        bundle.take_logical_device(device_id, oversized_budget),
        Err(rdif_block::BundleError::QueueLimitExceeded {
            device_id: rejected,
            max_queues: rdif_block::MAX_CONTROLLER_QUEUES,
        }) if rejected == device_id
    ));
    assert!(
        bundle.logical_device_ids().contains(device_id),
        "a rejected budget must not consume the compatibility device"
    );
}

#[derive(Clone, Copy)]
enum QueueScript {
    EmptyThenValid,
    InvalidThenValid,
}

struct ScriptedLegacyDevice {
    script: QueueScript,
    create_calls: usize,
    shutdowns: Arc<AtomicUsize>,
    shutdown_error: bool,
}

impl ScriptedLegacyDevice {
    fn empty_then_valid(shutdowns: Arc<AtomicUsize>) -> Self {
        Self {
            script: QueueScript::EmptyThenValid,
            create_calls: 0,
            shutdowns,
            shutdown_error: false,
        }
    }

    fn invalid_then_valid(shutdowns: Arc<AtomicUsize>) -> Self {
        Self {
            script: QueueScript::InvalidThenValid,
            create_calls: 0,
            shutdowns,
            shutdown_error: false,
        }
    }

    fn invalid_with_failed_shutdown(shutdowns: Arc<AtomicUsize>) -> Self {
        Self {
            script: QueueScript::InvalidThenValid,
            create_calls: 0,
            shutdowns,
            shutdown_error: true,
        }
    }

    fn create_scripted_queue(&mut self) -> Option<QueueHandle> {
        let create_call = self.create_calls;
        self.create_calls += 1;
        let matching_geometry = match (self.script, create_call) {
            (QueueScript::EmptyThenValid, 0) => return None,
            (QueueScript::EmptyThenValid, 1) | (QueueScript::InvalidThenValid, 1) => true,
            (QueueScript::InvalidThenValid, 0) => false,
            _ => return None,
        };
        let expected_device = DeviceInfo::new(64, 512);
        let queue_device = if matching_geometry {
            expected_device
        } else {
            DeviceInfo::new(63, 512)
        };
        let limits = QueueLimits::simple(512, u64::MAX);
        QueueHandle::new(Box::new(ShutdownTrackingQueue {
            info: QueueInfo {
                id: 9,
                device: queue_device,
                limits,
                kind: QueueKind::Inline,
                dispatch_mode: DispatchMode::Direct,
            },
            shutdowns: Arc::clone(&self.shutdowns),
            shutdown_error: self.shutdown_error,
        }))
        .into()
    }
}

impl DriverGeneric for ScriptedLegacyDevice {
    fn name(&self) -> &str {
        "scripted-legacy"
    }
}

impl rdif_block::Interface for ScriptedLegacyDevice {
    fn controller_init(&mut self) -> ControllerInitEndpoint<'_> {
        ControllerInitEndpoint::Ready
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Inline
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo::new(64, 512)
    }

    fn queue_limits(&self) -> QueueLimits {
        QueueLimits::simple(512, u64::MAX)
    }

    fn create_queue(&mut self) -> Option<QueueHandle> {
        self.create_scripted_queue()
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

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        Vec::new()
    }

    fn take_irq_handler(&mut self, _source_id: usize) -> Option<rdif_block::BIrqHandler> {
        None
    }
}

struct ShutdownTrackingQueue {
    info: QueueInfo,
    shutdowns: Arc<AtomicUsize>,
    shutdown_error: bool,
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
        if self.shutdown_error {
            Err(BlkError::Io)
        } else {
            Ok(())
        }
    }
}

fn logical_device(
    device_id: usize,
    device_blocks: u64,
    queue_id: usize,
    queue_blocks: u64,
) -> rdif_block::LogicalDevice {
    let device = DeviceInfo::new(device_blocks, 512);
    let limits = QueueLimits::simple(512, u64::MAX);
    let queue_info = QueueInfo {
        id: queue_id,
        device: DeviceInfo::new(queue_blocks, 512),
        limits,
        kind: QueueKind::Inline,
        dispatch_mode: DispatchMode::Direct,
    };
    rdif_block::LogicalDevice::new(
        LogicalDeviceId::new(device_id).unwrap(),
        format!("disk-{device_id}"),
        device,
        limits,
        vec![QueueHandle::new(Box::new(InlineQueue { info: queue_info }))],
    )
}

fn shutdown_devices(devices: Vec<rdif_block::LogicalDevice>) {
    for device in devices {
        for mut queue in device.into_parts().queues {
            queue.shutdown(&mut ShutdownSink).unwrap();
        }
    }
}
