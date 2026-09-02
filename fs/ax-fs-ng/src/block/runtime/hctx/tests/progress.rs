use super::*;

#[test]
fn negotiated_queue_depth_may_shrink_but_never_grow() {
    assert!(queue_info_fits_provisioned(
        test_queue_info(32),
        test_queue_info(8)
    ));
    assert!(queue_info_fits_provisioned(
        test_queue_info(32),
        test_queue_info(32)
    ));
    assert!(!queue_info_fits_provisioned(
        test_queue_info(8),
        test_queue_info(32)
    ));

    let mut invalid_batch = test_queue_info(8);
    invalid_batch.limits.max_inflight = 4;
    assert!(!queue_info_fits_provisioned(
        test_queue_info(8),
        invalid_batch,
    ));
}

#[test]
fn register_retry_advances_only_register_state_and_posts_controller_event() {
    crate::os::task::install_test_runtime_ops();
    let state = HctxState::test_new(test_queue_info(1), Vec::new());
    let retries = Arc::new(AtomicUsize::new(0));
    let drains = Arc::new(AtomicUsize::new(0));
    let mut queue = RegisterRetryQueue {
        retry_after: Some(Duration::from_millis(2)),
        retries: Arc::clone(&retries),
        drains: Arc::clone(&drains),
    };
    let controller = TestControllerPort::default();
    let now = Duration::from_secs(10);
    let mut retry_at = None;
    let mut deadline = None;
    let mut fatal_error = None;
    let mut pending = BTreeMap::new();
    let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
    let observer = Arc::downgrade(&observer);

    reconcile_register_retry(&queue, &mut retry_at, &mut deadline, now);
    assert_eq!(retry_at, Some(now + Duration::from_millis(2)));
    assert_eq!(deadline, Some(now + QUEUE_REGISTER_TRANSITION_TIMEOUT));
    assert!(!advance_register_retry_if_due(
        &mut queue,
        now + Duration::from_millis(1),
        &mut RegisterRetryContext {
            controller: &controller,
            pending: &mut pending,
            observer: &observer,
            retry_at: &mut retry_at,
            deadline: &mut deadline,
            state: &state,
            fatal_error: &mut fatal_error,
        },
    ));
    assert!(advance_register_retry_if_due(
        &mut queue,
        now + Duration::from_millis(2),
        &mut RegisterRetryContext {
            controller: &controller,
            pending: &mut pending,
            observer: &observer,
            retry_at: &mut retry_at,
            deadline: &mut deadline,
            state: &state,
            fatal_error: &mut fatal_error,
        },
    ));

    assert_eq!(retries.load(Ordering::Acquire), 1);
    assert_eq!(drains.load(Ordering::Acquire), 0);
    assert_eq!(fatal_error, None);
    assert_eq!(
        controller.events.lock().unwrap().as_slice(),
        [ControllerEvent::RegisterRetry]
    );
}

#[test]
fn terminal_state_rejects_an_already_due_register_retry() {
    crate::os::task::install_test_runtime_ops();
    let state = HctxState::test_new(test_queue_info(1), Vec::new());
    state.stopping.store(true, Ordering::Release);
    let retries = Arc::new(AtomicUsize::new(0));
    let drains = Arc::new(AtomicUsize::new(0));
    let mut queue = RegisterRetryQueue {
        retry_after: Some(Duration::from_millis(1)),
        retries: Arc::clone(&retries),
        drains,
    };
    let controller = TestControllerPort::default();
    let now = Duration::from_secs(10);
    let mut retry_at = Some(now);
    let mut deadline = Some(now + QUEUE_REGISTER_TRANSITION_TIMEOUT);
    let mut fatal_error = Some(BlkError::Io);
    let mut pending = BTreeMap::new();
    let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
    let observer = Arc::downgrade(&observer);

    assert!(!advance_register_retry_if_due(
        &mut queue,
        now,
        &mut RegisterRetryContext {
            controller: &controller,
            pending: &mut pending,
            observer: &observer,
            retry_at: &mut retry_at,
            deadline: &mut deadline,
            state: &state,
            fatal_error: &mut fatal_error,
        },
    ));

    assert_eq!(retries.load(Ordering::Acquire), 0);
    assert!(controller.events.lock().unwrap().is_empty());
}

#[test]
fn concurrent_stop_does_not_wait_for_a_single_consumer_notification() {
    crate::os::task::install_test_runtime_ops();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let hctx = Hctx::start(
        Box::new(BlockingShutdownQueue {
            entered: entered_tx,
            release: release_rx,
        }),
        0,
        Arc::downgrade(&observer),
        controller,
    )
    .unwrap();
    let first_hctx = Arc::clone(&hctx);
    let first = thread::spawn(move || first_hctx.stop());
    let entered = entered_rx.recv_timeout(Duration::from_secs(1));
    if entered.is_err() {
        let _ = release_tx.send(());
        let _ = first.join();
        panic!("first stop did not reach queue shutdown");
    }

    let (second_tx, second_rx) = mpsc::channel();
    let second = thread::spawn(move || {
        let _ = second_tx.send(hctx.stop());
    });
    let second_result = second_rx.recv_timeout(Duration::from_secs(1));
    release_tx.send(()).unwrap();
    assert_eq!(first.join().unwrap(), Ok(()));
    second.join().unwrap();

    assert_eq!(second_result, Ok(Err(BlkError::Io)));
}

#[test]
fn retry_backlog_does_not_starve_fresh_cpu_channel_submissions() {
    crate::os::task::install_test_runtime_ops();
    let ops = runtime_ops().unwrap();
    let notification = ops.notification();
    let channel =
        Arc::new(BoundedChannel::with_item_notification(4, Arc::clone(&notification)).unwrap());
    let state = HctxState::test_new(test_queue_info(2), vec![Arc::clone(&channel)]);
    let (_fresh_subscription, fresh) = flush_submission_at(100);
    assert!(channel.send(fresh, false).is_ok());
    let (_first_retry_subscription, first_retry) = flush_submission_at(1);
    let (_second_retry_subscription, second_retry) = flush_submission_at(2);
    let mut retries = VecDeque::from([first_retry, second_retry]);
    let mut next_channel = 0;
    let mut prefer_retry = true;
    let mut batch = VecDeque::with_capacity(2);

    collect_submission_batch(
        &state,
        &mut retries,
        &mut next_channel,
        &mut prefer_retry,
        2,
        &mut batch,
    );
    let lbas: Vec<_> = batch
        .into_iter()
        .map(|submission| submission.request.lba)
        .collect();

    assert_eq!(lbas, [1, 100]);
    assert_eq!(retries.front().unwrap().request.lba, 2);
}

#[test]
fn irq_drain_refreshes_hctx_queue_capabilities() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let initialized = Arc::new(AtomicBool::new(false));
    let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let queue = CapabilityRefreshQueue {
        counters: Arc::clone(&counters),
        initialized: Arc::clone(&initialized),
    };
    let hctx = Hctx::start(Box::new(queue), 0, Arc::downgrade(&observer), controller).unwrap();

    let initial = hctx.info();
    assert!(!initial.limits.supports_flush);
    assert_eq!(initial.limits.max_blocks_per_request, 256);

    let target = hctx.irq_target(0);
    let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
    assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);
    let deadline = Instant::now() + Duration::from_secs(1);
    while !hctx.info().limits.supports_flush {
        assert!(
            Instant::now() < deadline,
            "maintenance task did not publish identified queue capabilities"
        );
        thread::yield_now();
    }

    let refreshed = hctx.info();
    assert_eq!(counters.drained.load(Ordering::Acquire), 1);
    assert!(refreshed.limits.supports_flush);
    assert_eq!(refreshed.limits.max_blocks_per_request, 8192);
    assert_eq!(refreshed.id, initial.id);
    assert_eq!(refreshed.limits.max_inflight, initial.limits.max_inflight);
    assert_eq!(
        refreshed.limits.max_submit_batch,
        initial.limits.max_submit_batch
    );
    hctx.stop().unwrap();
}

#[test]
fn terminal_irq_drain_failure_does_not_advance_controller_or_rearm() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let observer = Arc::new(TestObserver::default());
    let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
    let controller = Arc::new(TestControllerPort::default());
    let controller_dyn: Arc<dyn ControllerEventPort> = controller.clone();
    let queue = FailingDrainQueue {
        counters: Arc::clone(&counters),
    };
    let hctx = Hctx::start(
        Box::new(queue),
        0,
        Arc::downgrade(&observer_dyn),
        controller_dyn,
    )
    .unwrap();

    let target = hctx.irq_target(0);
    let mut action = BlockIrqAction::new(Box::new(QueueZeroControlIrq), vec![target]);
    assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);

    let deadline = Instant::now() + Duration::from_secs(1);
    while observer.failed.load(Ordering::Acquire) != 1 {
        assert!(
            Instant::now() < deadline,
            "terminal queue failure did not stop the hctx"
        );
        thread::yield_now();
    }
    hctx.stop().unwrap();

    assert_eq!(counters.drained.load(Ordering::Acquire), 1);
    assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
    assert_eq!(
        controller.events.lock().unwrap().as_slice(),
        [ControllerEvent::Watchdog { queue_id: 0 }],
        "the hctx terminal owner must not forward the failed IRQ or rearm it"
    );
}

#[test]
fn missing_irq_times_out_without_completion_drain() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let observer = Arc::new(TestObserver::default());
    let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
    let controller = Arc::new(TestControllerPort::default());
    let queue = NeverCompletesQueue {
        counters: Arc::clone(&counters),
        next_id: 0,
        pending: Vec::new(),
    };
    let hctx = Hctx::start(
        Box::new(queue),
        0,
        Arc::downgrade(&observer_dyn),
        controller.clone(),
    )
    .unwrap();
    let channel = hctx.add_submission_channel().unwrap();
    let (subscription, submission) = flush_submission();
    assert!(channel.send(submission, false).is_ok());

    let completed = subscription.recv().unwrap();
    assert_eq!(completed.result, Err(BlkError::TimedOut));
    hctx.stop().unwrap();

    assert!(
        channel.is_closed(),
        "a terminal hctx must seal its submission channel before exiting"
    );
    assert_eq!(counters.drained.load(Ordering::Acquire), 0);
    assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
    assert_eq!(observer.failed.load(Ordering::Acquire), 1);
    assert!(
        controller
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| { *event == ControllerEvent::Watchdog { queue_id: 0 } })
    );
}
