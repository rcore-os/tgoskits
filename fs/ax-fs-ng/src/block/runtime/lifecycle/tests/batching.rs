use super::*;

#[test]
fn read_blocks_queues_the_next_bounded_window_before_waiting() {
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

    let counters = Arc::new(BatchingQueueCounters::default());
    let controller = BatchingReadController {
        queue: Some(BatchingReadQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
            fail_next_drain: false,
        }),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(12));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "batching-read",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    let reader = Arc::clone(&handle);
    let (result_tx, result_rx) = mpsc::channel();
    let read_thread = thread::spawn(move || {
        let mut buffer = vec![0; 8 * 512];
        result_tx.send(reader.read_blocks(0, &mut buffer)).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while counters.submitted.load(Ordering::Acquire) < 4 {
        assert!(
            Instant::now() < deadline,
            "synchronous wrapper waited before submitting the full I/O window"
        );
        thread::yield_now();
    }
    while counters.commits.load(Ordering::Acquire) < 1 {
        assert!(
            Instant::now() < deadline,
            "maintenance task did not commit the submitted I/O window"
        );
        thread::yield_now();
    }

    assert_eq!(counters.largest_batch.load(Ordering::Acquire), 4);
    assert_eq!(counters.commits.load(Ordering::Acquire), 1);
    while handle
        .inner
        .cpu_channels
        .lock()
        .iter()
        .map(|channel| channel.channel.queued_len())
        .sum::<usize>()
        < 4
    {
        assert!(
            Instant::now() < deadline,
            "the requester did not queue the second window before the first IRQ"
        );
        thread::yield_now();
    }
    assert_eq!(counters.submitted.load(Ordering::Acquire), 4);
    assert!(result_rx.try_recv().is_err());
    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    while counters.submitted.load(Ordering::Acquire) < 8 {
        assert!(
            Instant::now() < deadline,
            "maintenance task did not refill from the queued second window"
        );
        thread::yield_now();
    }
    while counters.commits.load(Ordering::Acquire) < 2 {
        assert!(
            Instant::now() < deadline,
            "maintenance task did not commit the refilled second window"
        );
        thread::yield_now();
    }
    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    read_thread.join().unwrap();
    assert_eq!(handle.shutdown(), 1);
}

#[cfg(feature = "ext4")]
#[test]
fn fua_write_marks_every_split_request() {
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

    let counters = Arc::new(BatchingQueueCounters::default());
    let controller = BatchingReadController {
        queue: Some(BatchingReadQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
            fail_next_drain: false,
        }),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(12));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "fua-write",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();
    assert!(handle.supports_fua());

    let writer = Arc::clone(&handle);
    let (result_tx, result_rx) = mpsc::channel();
    let write_thread = thread::spawn(move || {
        let buffer = vec![0x5a; 4 * 512];
        result_tx.send(writer.write_blocks_fua(0, &buffer)).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while counters.submitted.load(Ordering::Acquire) < 4 {
        assert!(
            Instant::now() < deadline,
            "FUA write requests were not submitted"
        );
        thread::yield_now();
    }
    while counters.commits.load(Ordering::Acquire) < 1 {
        assert!(
            Instant::now() < deadline,
            "maintenance task did not commit the FUA write window"
        );
        thread::yield_now();
    }
    assert_eq!(counters.fua_submitted.load(Ordering::Acquire), 4);
    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    assert!(
        result_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok()
    );
    write_thread.join().unwrap();
    assert_eq!(handle.shutdown(), 1);
}

#[test]
#[cfg(any(feature = "ext4", feature = "fat"))]
fn write_blocks_drains_submitted_windows_before_returning_error() {
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

    let counters = Arc::new(BatchingQueueCounters::default());
    let controller = BatchingReadController {
        queue: Some(BatchingReadQueue {
            counters: Arc::clone(&counters),
            next_id: 0,
            pending: Vec::new(),
            fail_next_drain: true,
        }),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(13));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "batching-write-error",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();

    let writer = Arc::clone(&handle);
    let (result_tx, result_rx) = mpsc::channel();
    let write_thread = thread::spawn(move || {
        let buffer = vec![0x42; 8 * 512];
        result_tx.send(writer.write_blocks(0, &buffer)).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while counters.submitted.load(Ordering::Acquire) < 4 {
        assert!(
            Instant::now() < deadline,
            "first write window was not submitted"
        );
        thread::yield_now();
    }
    while handle
        .inner
        .cpu_channels
        .lock()
        .iter()
        .map(|channel| channel.channel.queued_len())
        .sum::<usize>()
        < 4
    {
        assert!(
            Instant::now() < deadline,
            "second write window was not queued"
        );
        thread::yield_now();
    }

    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    while counters.submitted.load(Ordering::Acquire) < 8 {
        assert!(
            Instant::now() < deadline,
            "second write window was not submitted"
        );
        thread::yield_now();
    }
    assert!(
        result_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "write returned while an already submitted window was still in flight"
    );

    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    assert!(matches!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(BlockError::Device {
            stage: "complete window",
            operation: RequestOp::Write,
            lba: 0,
            source: BlkError::Io,
        })
    ));
    write_thread.join().unwrap();
    assert_eq!(handle.shutdown(), 1);
}
