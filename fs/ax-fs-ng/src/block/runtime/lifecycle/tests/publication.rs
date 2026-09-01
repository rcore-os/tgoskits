use super::*;

struct SerializedOnlineSmpController {
    queue: Option<LifecycleQueue>,
    online_entered: mpsc::Sender<usize>,
    online_release: mpsc::Receiver<()>,
}

struct DeviceInfoUpdateController {
    queue: Option<LifecycleQueue>,
    changed_info: DeviceInfo,
}

struct MutableQueueInfoController {
    queue: Option<MutableQueueInfoQueue>,
}

struct MutableQueueInfoQueue {
    info: Arc<StdMutex<QueueInfo>>,
}

struct ReadyPrefixController {
    queue: Option<IndexedLifecycleQueue>,
}

impl DriverGeneric for MutableQueueInfoQueue {
    fn name(&self) -> &str {
        "mutable-queue-info"
    }
}

impl HardwareQueue for MutableQueueInfoQueue {
    fn id(&self) -> usize {
        self.info.lock().unwrap().id
    }

    fn info(&self) -> QueueInfo {
        *self.info.lock().unwrap()
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
        Ok(())
    }
}

impl DriverGeneric for DeviceInfoUpdateController {
    fn name(&self) -> &str {
        "device-info-update-controller"
    }
}

impl BlockController for DeviceInfoUpdateController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.queue.take().ok_or(BlkError::Io)?)],
                Vec::new(),
            )),
            ControllerEvent::OnlineSmp { .. } => {
                Ok(ControllerUpdate::state(ControllerState::Ready)
                    .with_device_info(self.changed_info))
            }
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

impl DriverGeneric for MutableQueueInfoController {
    fn name(&self) -> &str {
        "mutable-queue-info-controller"
    }
}

impl BlockController for MutableQueueInfoController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.queue.take().ok_or(BlkError::Io)?)],
                vec![IrqEndpoint::new(0, 1, Box::new(QueueZeroHandler))],
            )),
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

impl DriverGeneric for ReadyPrefixController {
    fn name(&self) -> &str {
        "ready-prefix-controller"
    }
}

impl BlockController for ReadyPrefixController {
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
                vec![Box::new(self.queue.take().ok_or(BlkError::Io)?)],
                Vec::new(),
            )),
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

impl DriverGeneric for SerializedOnlineSmpController {
    fn name(&self) -> &str {
        "serialized-online-smp-controller"
    }
}

impl BlockController for SerializedOnlineSmpController {
    fn device_info(&self) -> DeviceInfo {
        test_queue_info().device
    }

    fn max_io_queues(&self) -> usize {
        1
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                ControllerState::Ready,
                vec![Box::new(self.queue.take().ok_or(BlkError::Io)?)],
                Vec::new(),
            )),
            ControllerEvent::OnlineSmp { target_queues } => {
                self.online_entered
                    .send(target_queues)
                    .map_err(|_| BlkError::Io)?;
                self.online_release.recv().map_err(|_| BlkError::Io)?;
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

#[test]
fn idempotent_online_smp_is_serialized_without_replacing_cpu_channels() {
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    let (online_entered_tx, online_entered_rx) = mpsc::channel();
    let (online_release_tx, online_release_rx) = mpsc::channel();
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "serialized-online-smp",
        [],
        Box::new(SerializedOnlineSmpController {
            queue: Some(LifecycleQueue { log }),
            online_entered: online_entered_tx,
            online_release: online_release_rx,
        }),
    ))
    .unwrap();
    assert_eq!(handle.inner.hctxs.lock().len(), 1);
    assert_eq!(handle.inner.cpu_channels.lock().len(), 1);
    let original_channel = Arc::clone(&handle.inner.cpu_channels.lock()[0].channel);

    let online_handle = Arc::clone(&handle);
    let online_thread = thread::spawn(move || online_handle.online_smp());

    assert_eq!(
        online_entered_rx.recv_timeout(Duration::from_secs(1)),
        Ok(1),
        "even an idempotent queue target must reach the controller thread"
    );
    assert_eq!(
        handle.inner.cpu_channels.lock().len(),
        1,
        "the caller must not publish a CPU mapping while the controller update is pending"
    );
    assert!(Arc::ptr_eq(
        &original_channel,
        &handle.inner.cpu_channels.lock()[0].channel,
    ));

    online_release_tx.send(()).unwrap();
    assert_eq!(online_thread.join().unwrap(), Ok(()));
    assert_eq!(handle.inner.cpu_channels.lock().len(), 1);
    assert!(Arc::ptr_eq(
        &original_channel,
        &handle.inner.cpu_channels.lock()[0].channel,
    ));
    assert_eq!(handle.inner.hctxs.lock()[0].submission_channel_count(), 1);

    for _ in 0..3 {
        online_release_tx.send(()).unwrap();
        assert_eq!(handle.online_smp(), Ok(()));
        assert_eq!(
            online_entered_rx.recv_timeout(Duration::from_secs(1)),
            Ok(1),
            "every idempotent update must still reach the controller thread"
        );
        assert!(Arc::ptr_eq(
            &original_channel,
            &handle.inner.cpu_channels.lock()[0].channel,
        ));
        assert_eq!(handle.inner.hctxs.lock()[0].submission_channel_count(), 1);
    }
    assert_eq!(handle.shutdown(), 0);
}

#[test]
fn provisional_hctx_is_promoted_only_by_a_ready_update() {
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "ready-prefix",
        [],
        Box::new(ReadyPrefixController {
            queue: Some(IndexedLifecycleQueue {
                id: 0,
                log: Arc::clone(&log),
            }),
        }),
    ))
    .unwrap();
    assert_eq!(
        handle
            .inner
            .lifecycle_gate
            .lock()
            .submission_ready_hctx_count,
        1
    );

    let mut provisional = ControllerUpdate::with_resources(
        ControllerState::WaitingForIrq,
        vec![Box::new(IndexedLifecycleQueue { id: 1, log })],
        Vec::new(),
    );
    assert_eq!(
        handle
            .inner
            .install_update(&mut provisional, Arc::clone(&handle.inner.controller),),
        Ok(Vec::new())
    );
    assert_eq!(handle.inner.hctxs.lock().len(), 2);
    assert_eq!(
        handle
            .inner
            .lifecycle_gate
            .lock()
            .submission_ready_hctx_count,
        1
    );

    let mut ready = ControllerUpdate::state(ControllerState::Ready);
    assert_eq!(
        handle
            .inner
            .install_update(&mut ready, Arc::clone(&handle.inner.controller)),
        Ok(Vec::new())
    );
    assert_eq!(
        handle
            .inner
            .lifecycle_gate
            .lock()
            .submission_ready_hctx_count,
        2
    );
}

#[test]
fn ready_device_rejects_changed_device_info_without_overwriting_epoch() {
    crate::os::task::install_test_runtime_ops();
    let initial_info = test_queue_info().device;
    let changed_info = DeviceInfo {
        num_blocks: initial_info.num_blocks + 1,
        ..initial_info
    };
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "device-info-freeze",
        [],
        Box::new(DeviceInfoUpdateController {
            queue: Some(LifecycleQueue {
                log: Arc::new(StdMutex::new(Vec::new())),
            }),
            changed_info,
        }),
    ))
    .unwrap();

    assert_eq!(
        handle
            .inner
            .controller
            .call(ControllerEvent::OnlineSmp { target_queues: 1 }),
        Err(BlkError::InvalidRequest)
    );
    assert_eq!(handle.device_info().num_blocks, initial_info.num_blocks);
    assert_eq!(handle.shutdown(), 0);
}

#[test]
fn frozen_device_info_rejects_every_identity_and_geometry_change() {
    let baseline = test_queue_info().device;
    let changes = [
        DeviceInfo {
            num_blocks: baseline.num_blocks + 1,
            ..baseline
        },
        DeviceInfo {
            logical_block_size: baseline.logical_block_size * 2,
            ..baseline
        },
        DeviceInfo {
            physical_block_size: baseline.physical_block_size * 2,
            ..baseline
        },
        DeviceInfo {
            read_only: !baseline.read_only,
            ..baseline
        },
        DeviceInfo {
            name: Some("changed-name"),
            ..baseline
        },
        DeviceInfo {
            vendor: Some("changed-vendor"),
            ..baseline
        },
        DeviceInfo {
            model: Some("changed-model"),
            ..baseline
        },
    ];

    for observed in changes {
        let mut epoch = DeviceInfoEpoch::new(baseline);
        epoch.freeze();
        assert_eq!(epoch.observe(observed), Err(BlkError::InvalidRequest));
        assert_eq!(epoch.published(), baseline);
    }

    let mut epoch = DeviceInfoEpoch::new(baseline);
    epoch.freeze();
    assert_eq!(epoch.observe(baseline), Ok(()));
}

#[test]
fn starting_device_info_tracks_discovery_until_ready_freezes_it() {
    let initial = DeviceInfo::new(0, 512);
    let discovered = DeviceInfo {
        num_blocks: 4096,
        name: Some("discovered-device"),
        vendor: Some("test-vendor"),
        model: Some("test-model"),
        ..initial
    };
    let mut epoch = DeviceInfoEpoch::new(initial);

    assert_eq!(epoch.observe(discovered), Ok(()));
    assert_eq!(epoch.published(), discovered);

    epoch.freeze();
    assert_eq!(epoch.observe(discovered), Ok(()));
    assert_eq!(
        epoch.observe(DeviceInfo {
            num_blocks: discovered.num_blocks + 1,
            ..discovered
        }),
        Err(BlkError::InvalidRequest)
    );
    assert_eq!(epoch.published(), discovered);
}

#[test]
fn ready_hctx_rejects_changed_dma_coherency() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(log);
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let queue_info = Arc::new(StdMutex::new(test_queue_info()));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "queue-info-freeze",
        [BlockIrqSource {
            source_id: 0,
            irq: IrqId::new(IrqDomainId(1), HwIrq(16)),
        }],
        Box::new(MutableQueueInfoController {
            queue: Some(MutableQueueInfoQueue {
                info: Arc::clone(&queue_info),
            }),
        }),
    ))
    .unwrap();

    {
        let mut changed = queue_info.lock().unwrap();
        changed.limits.dma = dma_api::DmaDeviceInfo::new(
            changed.limits.dma.domain(),
            dma_api::DmaCoherency::Coherent,
            changed.limits.dma.constraints(),
        );
    }
    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    wait_for_device_teardown(&handle.inner);
    assert!(handle.inner.controller.terminal_confirmed());
    assert_eq!(handle.shutdown(), 0);
}

#[test]
fn runtime_admission_returns_request_with_mismatched_dma_coherency() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    install_dma_op(&TEST_DMA_OP);
    let log = Arc::new(StdMutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(log);
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let prepared_info = test_queue_info();
    let prepared = crate::block::runtime::dma::prepare_read(prepared_info.limits, 512).unwrap();
    let mut queue_info = prepared_info;
    queue_info.limits.dma = dma_api::DmaDeviceInfo::new(
        queue_info.limits.dma.domain(),
        dma_api::DmaCoherency::Coherent,
        queue_info.limits.dma.constraints(),
    );
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "runtime-coherency-validation",
        [BlockIrqSource {
            source_id: 0,
            irq: IrqId::new(IrqDomainId(1), HwIrq(17)),
        }],
        Box::new(MutableQueueInfoController {
            queue: Some(MutableQueueInfoQueue {
                info: Arc::new(StdMutex::new(queue_info)),
            }),
        }),
    ))
    .unwrap();
    let request = OwnedRequest {
        op: RequestOp::Read,
        lba: 0,
        block_count: 1,
        data: Some(prepared),
        flags: RequestFlags::NONE,
    };

    let error = match handle.submit_owned(request) {
        Ok(_) => panic!("runtime accepted DMA prepared for a different coherency mode"),
        Err(error) => error,
    };
    assert_eq!(error.error, BlkError::InvalidRequest);
    assert_eq!(handle.inner.lifecycle_gate.lock().active_data, 0);
    let returned = error.into_request();
    assert!(returned.data.is_some());
    drop(crate::block::runtime::dma::complete_without_submit(
        returned.data,
    ));
    assert_eq!(handle.shutdown(), 1);
}
