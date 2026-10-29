use super::*;

#[test]
fn read_blocks_queues_the_next_bounded_window_before_waiting() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    install_dma_op(&TEST_DMA_OP);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
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
            reported_info: batching_queue_info(),
            next_id: 0,
            pending: Vec::new(),
            fail_next_drain: false,
            probe: None,
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
        .lock_irqsave()
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
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
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
            reported_info: batching_queue_info(),
            next_id: 0,
            pending: Vec::new(),
            fail_next_drain: false,
            probe: None,
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
    failed_write_drains_windows(WriteScenario::Ordinary);
    #[cfg(feature = "ext4")]
    {
        failed_write_drains_windows(WriteScenario::ZeroDma);
        failed_write_drains_windows(WriteScenario::ZeroNative);
    }
}

#[cfg(any(feature = "ext4", feature = "fat"))]
enum WriteScenario {
    Ordinary,
    #[cfg(feature = "ext4")]
    ZeroDma,
    #[cfg(feature = "ext4")]
    ZeroNative,
}

/// Both ordinary and range-zeroing writes must join already accepted I/O.
#[cfg(any(feature = "ext4", feature = "fat"))]
fn failed_write_drains_windows(scenario: WriteScenario) {
    let (zeroes, native) = match scenario {
        WriteScenario::Ordinary => (false, false),
        #[cfg(feature = "ext4")]
        WriteScenario::ZeroDma => (true, false),
        #[cfg(feature = "ext4")]
        WriteScenario::ZeroNative => (true, true),
    };
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    install_dma_op(&TEST_DMA_OP);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(log);
    *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
    TEST_IRQ_REGISTRAR
        .fail_registration
        .store(false, Ordering::Release);
    set_irq_registrar(&TEST_IRQ_REGISTRAR);

    let counters = Arc::new(BatchingQueueCounters::default());
    let mut info = batching_queue_info();
    if native {
        info.limits.max_write_zeroes_blocks = 4;
    }
    let controller = BatchingReadController {
        queue: Some(BatchingReadQueue {
            counters: Arc::clone(&counters),
            reported_info: info,
            next_id: 0,
            pending: Vec::new(),
            fail_next_drain: true,
            probe: None,
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
        DMA_ALLOCATIONS.set(0);
        let result = match zeroes {
            #[cfg(feature = "ext4")]
            true if native => writer.write_zeroes(&[
                crate::block::BlockRegion::new(0, 7),
                crate::block::BlockRegion::new(16, 7),
            ]),
            #[cfg(feature = "ext4")]
            true => writer.write_zeroes(
                &(0..8)
                    .map(|block| crate::block::BlockRegion::new(block * 2, 1))
                    .collect::<Vec<_>>(),
            ),
            _ => writer.write_blocks(0, &buffer),
        };
        result_tx.send((result, DMA_ALLOCATIONS.get())).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    let first_window = if native { 1 } else { 4 };
    while counters.submitted.load(Ordering::Acquire) < first_window {
        assert!(
            Instant::now() < deadline,
            "first write window was not submitted"
        );
        thread::yield_now();
    }
    assert!(
        result_rx.try_recv().is_err(),
        "writing must wait for the accepted request's completion IRQ"
    );
    if !native {
        while handle
            .inner
            .cpu_channels
            .lock_irqsave()
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
    }
    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    if !native {
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
    }
    let (result, allocations) = result_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    match result {
        Err(BlockError::Device {
            stage,
            operation,
            lba,
            source,
        }) => {
            assert_eq!(
                stage,
                if native {
                    "complete native zero"
                } else {
                    "complete window"
                }
            );
            assert_eq!(
                operation,
                if native {
                    RequestOp::WriteZeroes
                } else {
                    RequestOp::Write
                }
            );
            assert_eq!(lba, 0);
            assert_eq!(source, BlkError::Io);
        }
        other => panic!("failed completion must propagate from zeroing/writing: {other:?}"),
    }
    let expected_submitted = if native { 1 } else { 8 };
    assert_eq!(
        counters.submitted.load(Ordering::Acquire),
        expected_submitted
    );
    assert_eq!(
        counters.completed.load(Ordering::Acquire),
        expected_submitted,
        "returning an error must leave no accepted I/O live"
    );
    if native {
        assert_eq!(allocations, 0);
        assert_eq!(
            *counters.native_zeroes.lock().unwrap(),
            [(0, 4)],
            "failed native completion must stop before the remaining range"
        );
        assert!(
            counters.writes.lock().unwrap().is_empty(),
            "failed native zeroing must not replay ordinary writes"
        );
    }
    write_thread.join().unwrap();
    assert_eq!(handle.shutdown(), 1);
}

#[cfg(feature = "ext4")]
#[test]
fn zero_ranges_pipeline_across_gaps_and_recycle_completed_dma() {
    for native in [false, true] {
        let _registrar_guard = lock_test_irq_registrar();
        crate::os::task::install_test_runtime_ops();
        install_dma_op(&TEST_DMA_OP);
        *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::new(std::sync::Mutex::new(Vec::new())));
        *TEST_IRQ_REGISTRAR.action.lock().unwrap() = None;
        TEST_IRQ_REGISTRAR
            .fail_registration
            .store(false, Ordering::Release);
        set_irq_registrar(&TEST_IRQ_REGISTRAR);
        let counters = Arc::new(BatchingQueueCounters::default());
        let mut info = batching_queue_info();
        if native {
            info.limits.max_write_zeroes_blocks = 4;
        }
        let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
            "batch-zero-ranges",
            [BlockIrqSource {
                source_id: 0,
                irq: IrqId::new(IrqDomainId(1), HwIrq(14)),
            }],
            Box::new(BatchingReadController {
                queue: Some(BatchingReadQueue {
                    counters: Arc::clone(&counters),
                    reported_info: info,
                    next_id: 0,
                    pending: Vec::new(),
                    fail_next_drain: false,
                    probe: None,
                }),
            }),
        ))
        .unwrap();
        // A bad suffix must be rejected before admitting the valid prefix.
        assert!(
            handle
                .write_zeroes(&[
                    crate::block::BlockRegion::new(0, 1),
                    crate::block::BlockRegion::new(32, 1),
                ])
                .is_err()
        );
        assert_eq!(counters.submitted.load(Ordering::Acquire), 0);
        let writer = Arc::clone(&handle);
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let ranges: Vec<_> = if native {
                vec![
                    crate::block::BlockRegion::new(0, 7),
                    crate::block::BlockRegion::new(16, 7),
                ]
            } else {
                (0..16)
                    .map(|block| crate::block::BlockRegion::new(block * 2, 1))
                    .collect()
            };
            DMA_ALLOCATIONS.set(0);
            let result = writer.write_zeroes(&ranges);
            tx.send((result, DMA_ALLOCATIONS.get())).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let requests_per_window = if native { 1 } else { 4 };
        for window in 1..=4 {
            while counters.submitted.load(Ordering::Acquire) < window * requests_per_window {
                assert!(
                    Instant::now() < deadline,
                    "range pipeline stopped before hardware completion"
                );
                thread::yield_now();
            }
            while counters.commits.load(Ordering::Acquire) < window {
                assert!(
                    Instant::now() < deadline,
                    "zero window was not committed to hardware"
                );
                thread::yield_now();
            }
            if window == 1 && !native {
                while handle
                    .inner
                    .cpu_channels
                    .lock_irqsave()
                    .iter()
                    .map(|channel| channel.channel.queued_len())
                    .sum::<usize>()
                    < 4
                {
                    assert!(
                        Instant::now() < deadline,
                        "next range window must queue before the first IRQ"
                    );
                    thread::yield_now();
                }
                assert!(rx.try_recv().is_err());
            }
            if native {
                assert_eq!(
                    counters.submitted.load(Ordering::Acquire),
                    window,
                    "the next native range must wait for terminal completion"
                );
                assert!(
                    handle
                        .inner
                        .cpu_channels
                        .lock_irqsave()
                        .iter()
                        .all(|channel| channel.channel.queued_len() == 0)
                );
                assert!(
                    rx.try_recv().is_err(),
                    "zeroing returned before its completion IRQ"
                );
            }
            assert_eq!(
                TEST_IRQ_REGISTRAR.run_registered_action(),
                BlockIrqOutcome::Wake
            );
        }
        let (result, allocations) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        result.unwrap();
        thread.join().unwrap();
        if native {
            assert_eq!(allocations, 0, "native zeroing must bypass DMA preparation");
            assert_eq!(
                *counters.native_zeroes.lock().unwrap(),
                [(0, 4), (4, 3), (16, 4), (20, 3)],
                "native limits must split exact ranges independently of DMA limits"
            );
            assert!(counters.writes.lock().unwrap().is_empty());
            assert_eq!(counters.zero_payloads.load(Ordering::Acquire), 0);
            assert_eq!(counters.peak_pending.load(Ordering::Acquire), 1);
            let stats = handle.io_stats();
            assert_eq!(stats.writes, 4);
            assert_eq!(stats.sectors_written, 14);
            assert_eq!(stats.inflight, 0);
        } else {
            assert_eq!(
                allocations, 8,
                "sixteen requests must use only two windows of fresh DMA backing"
            );
            assert_eq!(
                *counters.writes.lock().unwrap(),
                (0..16).map(|block| block * 2).collect::<Vec<_>>()
            );
            assert_eq!(counters.zero_payloads.load(Ordering::Acquire), 16);
            assert!(counters.native_zeroes.lock().unwrap().is_empty());
        }
        assert_eq!(
            counters.completed.load(Ordering::Acquire),
            counters.submitted.load(Ordering::Acquire)
        );
        assert_eq!(handle.shutdown(), 1);
    }
}
