use super::*;

pub(super) struct DropTrackedQueue {
    info: QueueInfo,
    drop_event: &'static str,
    log: Arc<StdMutex<Vec<&'static str>>>,
    shutdown_error: Option<BlkError>,
}

impl DropTrackedQueue {
    pub(super) fn startable(
        id: usize,
        drop_event: &'static str,
        log: Arc<StdMutex<Vec<&'static str>>>,
    ) -> Self {
        Self {
            info: QueueInfo {
                id,
                ..test_queue_info()
            },
            drop_event,
            log,
            shutdown_error: None,
        }
    }

    pub(super) fn shutdown_failure(
        id: usize,
        drop_event: &'static str,
        log: Arc<StdMutex<Vec<&'static str>>>,
        error: BlkError,
    ) -> Self {
        Self {
            info: QueueInfo {
                id,
                ..test_queue_info()
            },
            drop_event,
            log,
            shutdown_error: Some(error),
        }
    }

    fn invalid_limits(
        id: usize,
        drop_event: &'static str,
        log: Arc<StdMutex<Vec<&'static str>>>,
    ) -> Self {
        let mut info = QueueInfo {
            id,
            ..test_queue_info()
        };
        info.limits.max_inflight = 0;
        info.limits.max_submit_batch = 0;
        Self {
            info,
            drop_event,
            log,
            shutdown_error: None,
        }
    }
}

impl Drop for DropTrackedQueue {
    fn drop(&mut self) {
        self.log.lock().unwrap().push(self.drop_event);
    }
}

impl HardwareQueue for DropTrackedQueue {
    fn id(&self) -> usize {
        self.info.id
    }

    fn info(&self) -> QueueInfo {
        self.info
    }

    fn submit_batch_owned(
        &mut self,
        _requests: &mut OwnedRequestBatch,
        _sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        BatchSubmitResult::new(0, BatchSubmitDisposition::QueueFull)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        Ok(())
    }

    fn drain_completions(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        Ok(())
    }

    fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        self.shutdown_error.take().map_or(Ok(()), Err)
    }
}

struct DropTrackedHandler {
    log: Arc<StdMutex<Vec<&'static str>>>,
}

impl Drop for DropTrackedHandler {
    fn drop(&mut self) {
        self.log.lock().unwrap().push("unregistered_endpoint_drop");
    }
}

impl HardIrqHandler for DropTrackedHandler {
    fn ack(&mut self) -> IrqAck {
        IrqAck::spurious(1)
    }
}

struct RejectedResourceUpdateController {
    bootstrap_queue: Option<LifecycleQueue>,
    emitted_queue: Option<DropTrackedQueue>,
    emitted_handler: Option<DropTrackedHandler>,
    changed_info: DeviceInfo,
    log: Arc<StdMutex<Vec<&'static str>>>,
}

struct RejectedQueueBatchController {
    bootstrap_queue: Option<LifecycleQueue>,
    emitted_queues: Option<Vec<Box<dyn HardwareQueue>>>,
    log: Arc<StdMutex<Vec<&'static str>>>,
}

impl DriverGeneric for RejectedResourceUpdateController {
    fn name(&self) -> &str {
        "rejected-resource-update-controller"
    }
}

impl BlockController for RejectedResourceUpdateController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        2
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.bootstrap_queue.take().ok_or(BlkError::Io)?)],
                Vec::new(),
            )),
            ControllerEvent::OnlineSmp { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.emitted_queue.take().ok_or(BlkError::Io)?)],
                vec![IrqEndpoint::new(
                    1,
                    1 << 1,
                    Box::new(self.emitted_handler.take().ok_or(BlkError::Io)?),
                )],
            )
            .with_device_info(self.changed_info)),
            ControllerEvent::QuiesceIrqs => {
                self.log.lock().unwrap().push("controller_quiesce");
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                self.log.lock().unwrap().push("controller_shutdown");
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

impl DriverGeneric for RejectedQueueBatchController {
    fn name(&self) -> &str {
        "rejected-queue-batch-controller"
    }
}

impl BlockController for RejectedQueueBatchController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        3
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.bootstrap_queue.take().ok_or(BlkError::Io)?)],
                Vec::new(),
            )),
            ControllerEvent::OnlineSmp { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                self.emitted_queues.take().ok_or(BlkError::Io)?,
                Vec::new(),
            )),
            ControllerEvent::QuiesceIrqs => {
                self.log.lock().unwrap().push("controller_quiesce");
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                self.log.lock().unwrap().push("controller_shutdown");
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

#[test]
fn rejected_device_info_update_keeps_emitted_queue_until_controller_shutdown() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    TEST_IRQ_REGISTRAR
        .fail_enable_at
        .store(usize::MAX, Ordering::Release);
    TEST_IRQ_FAIL_SYNCHRONIZE.store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);
    let initial_info = test_queue_info().device;
    let changed_info = DeviceInfo {
        num_blocks: initial_info.num_blocks + 1,
        ..initial_info
    };
    let controller = RejectedResourceUpdateController {
        bootstrap_queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        emitted_queue: Some(DropTrackedQueue::startable(
            1,
            "emitted_queue_drop",
            Arc::clone(&log),
        )),
        emitted_handler: Some(DropTrackedHandler {
            log: Arc::clone(&log),
        }),
        changed_info,
        log: Arc::clone(&log),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(12));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "rejected-resource-update",
        [BlockIrqSource { source_id: 1, irq }],
        Box::new(controller),
    ))
    .unwrap();

    assert_eq!(handle.online_smp(), Err(BlkError::InvalidRequest));
    assert_eq!(handle.device_info().num_blocks, initial_info.num_blocks);

    let log = log.lock().unwrap();
    let controller = log_position(&log, "controller_shutdown");
    let queue = log_position(&log, "emitted_queue_drop");
    assert!(log.contains(&"unregistered_endpoint_drop"));
    assert!(controller < queue);
}

fn assert_rejected_queue_batch_is_retained(
    log: Arc<StdMutex<Vec<&'static str>>>,
    emitted_queues: Vec<Box<dyn HardwareQueue>>,
    expected_drop_events: &[&str],
) {
    crate::os::task::install_test_runtime_ops();
    let controller = RejectedQueueBatchController {
        bootstrap_queue: Some(LifecycleQueue {
            log: Arc::clone(&log),
        }),
        emitted_queues: Some(emitted_queues),
        log: Arc::clone(&log),
    };
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "rejected-queue-batch",
        [],
        Box::new(controller),
    ))
    .unwrap();

    assert_eq!(handle.online_smp(), Err(BlkError::InvalidRequest));

    let log = log.lock().unwrap();
    let controller_shutdown = log_position(&log, "controller_shutdown");
    for event in expected_drop_events {
        assert!(
            controller_shutdown < log_position(&log, event),
            "queue event {event} occurred before controller shutdown: {log:?}"
        );
    }
}

#[test]
fn duplicate_queue_update_keeps_current_and_trailing_queues_until_controller_shutdown() {
    let log = Arc::new(StdMutex::new(Vec::new()));
    assert_rejected_queue_batch_is_retained(
        Arc::clone(&log),
        vec![
            Box::new(DropTrackedQueue::startable(
                0,
                "duplicate_queue_drop",
                Arc::clone(&log),
            )),
            Box::new(DropTrackedQueue::startable(
                1,
                "trailing_queue_drop",
                Arc::clone(&log),
            )),
        ],
        &["duplicate_queue_drop", "trailing_queue_drop"],
    );
}

#[test]
fn failed_hctx_start_keeps_current_and_trailing_queues_until_controller_shutdown() {
    let log = Arc::new(StdMutex::new(Vec::new()));
    assert_rejected_queue_batch_is_retained(
        Arc::clone(&log),
        vec![
            Box::new(DropTrackedQueue::invalid_limits(
                1,
                "invalid_queue_drop",
                Arc::clone(&log),
            )),
            Box::new(DropTrackedQueue::startable(
                2,
                "trailing_queue_drop",
                Arc::clone(&log),
            )),
        ],
        &["invalid_queue_drop", "trailing_queue_drop"],
    );
}
