use super::*;

#[test]
fn out_of_order_irq_completions_reach_the_right_subscriptions() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let queue = ReverseCompletionQueue {
        counters: Arc::clone(&counters),
        next_id: 0,
        pending: Vec::new(),
        accept_limit: 2,
        fatal_after_accept: false,
        fail_commit: false,
        inject_unexpected_completion: false,
    };
    let hctx = Hctx::start(Box::new(queue), 0, Arc::downgrade(&observer), controller).unwrap();
    let channel = hctx.add_submission_channel().unwrap();
    let (first, first_submission) = flush_submission();
    let (second, second_submission) = flush_submission();
    assert!(
        channel
            .send_many(VecDeque::from([first_submission, second_submission]), false,)
            .is_ok()
    );
    wait_for_submissions(&counters, 2);
    wait_for_commits(&counters, 1);
    assert_eq!(counters.committed.load(Ordering::Acquire), 1);

    let target = hctx.irq_target(0);
    let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
    assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);

    assert_eq!(usize::from(first.recv().unwrap().id), 1);
    assert_eq!(usize::from(second.recv().unwrap().id), 2);
    assert_eq!(counters.drained.load(Ordering::Acquire), 1);
    hctx.stop().unwrap();
}

#[test]
fn dropped_subscription_does_not_cancel_hardware_ownership() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let observer = Arc::new(TestObserver::default());
    let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let queue = ReverseCompletionQueue {
        counters: Arc::clone(&counters),
        next_id: 0,
        pending: Vec::new(),
        accept_limit: 2,
        fatal_after_accept: false,
        fail_commit: false,
        inject_unexpected_completion: false,
    };
    let hctx = Hctx::start(
        Box::new(queue),
        0,
        Arc::downgrade(&observer_dyn),
        controller,
    )
    .unwrap();
    let channel = hctx.add_submission_channel().unwrap();
    let (subscription, submission) = flush_submission();
    assert!(channel.send(submission, false).is_ok());
    wait_for_submissions(&counters, 1);
    drop(subscription);

    let target = hctx.irq_target(0);
    let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
    assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);
    let deadline = Instant::now() + Duration::from_secs(1);
    while observer.completed.load(Ordering::Acquire) != 1 {
        assert!(
            Instant::now() < deadline,
            "dropped subscription prevented deferred completion"
        );
        thread::yield_now();
    }

    assert_eq!(counters.drained.load(Ordering::Acquire), 1);
    hctx.stop().unwrap();
}

#[test]
fn partial_batch_is_committed_and_remaining_request_is_retried_after_irq() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let observer: Arc<dyn HctxObserver> = Arc::new(TestObserver::default());
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let queue = ReverseCompletionQueue {
        counters: Arc::clone(&counters),
        next_id: 0,
        pending: Vec::new(),
        accept_limit: 1,
        fatal_after_accept: false,
        fail_commit: false,
        inject_unexpected_completion: false,
    };
    let hctx = Hctx::start(Box::new(queue), 0, Arc::downgrade(&observer), controller).unwrap();
    let channel = hctx.add_submission_channel().unwrap();
    let (first, first_submission) = flush_submission();
    let (second, second_submission) = flush_submission();
    assert!(
        channel
            .send_many(VecDeque::from([first_submission, second_submission]), false,)
            .is_ok()
    );
    wait_for_submissions(&counters, 1);
    wait_for_commits(&counters, 1);
    assert_eq!(counters.submitted.load(Ordering::Acquire), 1);

    let target = hctx.irq_target(0);
    let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
    assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);
    wait_for_submissions(&counters, 2);
    wait_for_commits(&counters, 2);
    assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);

    assert!(first.recv().unwrap().result.is_ok());
    assert!(second.recv().unwrap().result.is_ok());
    assert_eq!(counters.submitted.load(Ordering::Acquire), 2);
    assert_eq!(counters.committed.load(Ordering::Acquire), 2);
    hctx.stop().unwrap();
}

#[test]
fn malformed_acceptance_report_still_terminates_every_runtime_request() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let observer = Arc::new(TestObserver::default());
    let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let queue = UnderreportedAcceptanceQueue {
        counters: Arc::clone(&counters),
        pending: Vec::new(),
    };
    let hctx = Hctx::start(
        Box::new(queue),
        0,
        Arc::downgrade(&observer_dyn),
        controller,
    )
    .unwrap();
    let channel = hctx.add_submission_channel().unwrap();
    let (_first, first_submission) = flush_submission();
    let (_second, second_submission) = flush_submission();
    assert!(
        channel
            .send_many(VecDeque::from([first_submission, second_submission]), false,)
            .is_ok()
    );

    let deadline = Instant::now() + Duration::from_secs(1);
    while observer.failed.load(Ordering::Acquire) != 1 {
        assert!(
            Instant::now() < deadline,
            "malformed queue contract did not fail the hctx"
        );
        thread::yield_now();
    }
    assert_eq!(hctx.stop(), Err(BlkError::Io));

    assert_eq!(counters.committed.load(Ordering::Acquire), 1);
    assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
    assert_eq!(counters.dropped.load(Ordering::Acquire), 1);
    assert_eq!(observer.completed.load(Ordering::Acquire), 2);
}

#[test]
fn accepted_prefix_is_committed_before_fatal_batch_teardown() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let observer = Arc::new(TestObserver::default());
    let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let queue = ReverseCompletionQueue {
        counters: Arc::clone(&counters),
        next_id: 0,
        pending: Vec::new(),
        accept_limit: 1,
        fatal_after_accept: true,
        fail_commit: false,
        inject_unexpected_completion: false,
    };
    let hctx = Hctx::start(
        Box::new(queue),
        0,
        Arc::downgrade(&observer_dyn),
        controller,
    )
    .unwrap();
    let channel = hctx.add_submission_channel().unwrap();
    let (_accepted, accepted_submission) = flush_submission();
    let (_remaining, remaining_submission) = flush_submission();
    assert!(
        channel
            .send_many(
                VecDeque::from([accepted_submission, remaining_submission]),
                false,
            )
            .is_ok()
    );

    let deadline = Instant::now() + Duration::from_secs(1);
    while observer.failed.load(Ordering::Acquire) != 1 {
        assert!(
            Instant::now() < deadline,
            "fatal submission result did not stop the hctx"
        );
        thread::yield_now();
    }
    hctx.stop().unwrap();

    assert_eq!(counters.submitted.load(Ordering::Acquire), 1);
    assert_eq!(counters.committed.load(Ordering::Acquire), 1);
    assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
    assert_eq!(observer.completed.load(Ordering::Acquire), 2);
}

#[test]
fn commit_failure_terminates_every_accepted_request() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let observer = Arc::new(TestObserver::default());
    let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let queue = ReverseCompletionQueue {
        counters: Arc::clone(&counters),
        next_id: 0,
        pending: Vec::new(),
        accept_limit: 2,
        fatal_after_accept: false,
        fail_commit: true,
        inject_unexpected_completion: false,
    };
    let hctx = Hctx::start(
        Box::new(queue),
        0,
        Arc::downgrade(&observer_dyn),
        controller,
    )
    .unwrap();
    let channel = hctx.add_submission_channel().unwrap();
    let (first, first_submission) = flush_submission();
    let (second, second_submission) = flush_submission();
    assert!(
        channel
            .send_many(VecDeque::from([first_submission, second_submission]), false,)
            .is_ok()
    );

    assert_eq!(first.recv().unwrap().result, Err(BlkError::Io));
    assert_eq!(second.recv().unwrap().result, Err(BlkError::Io));
    hctx.stop().unwrap();

    assert_eq!(counters.submitted.load(Ordering::Acquire), 2);
    assert_eq!(counters.committed.load(Ordering::Acquire), 1);
    assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
    assert_eq!(observer.completed.load(Ordering::Acquire), 2);
    assert_eq!(observer.failed.load(Ordering::Acquire), 1);
}

#[test]
fn unexpected_completion_fails_hctx_and_preserves_pending_ownership() {
    crate::os::task::install_test_runtime_ops();
    let counters = Arc::new(QueueCounters::default());
    let observer = Arc::new(TestObserver::default());
    let observer_dyn: Arc<dyn HctxObserver> = observer.clone();
    let controller: Arc<dyn ControllerEventPort> = Arc::new(TestControllerPort::default());
    let queue = ReverseCompletionQueue {
        counters: Arc::clone(&counters),
        next_id: 0,
        pending: Vec::new(),
        accept_limit: 1,
        fatal_after_accept: false,
        fail_commit: false,
        inject_unexpected_completion: true,
    };
    let hctx = Hctx::start(
        Box::new(queue),
        0,
        Arc::downgrade(&observer_dyn),
        controller,
    )
    .unwrap();
    let channel = hctx.add_submission_channel().unwrap();
    let (subscription, submission) = flush_submission();
    assert!(channel.send(submission, false).is_ok());
    wait_for_submissions(&counters, 1);

    let target = hctx.irq_target(0);
    let mut action = BlockIrqAction::new(Box::new(QueueZeroIrq), vec![target]);
    assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);

    assert_eq!(subscription.recv().unwrap().result, Err(BlkError::Io));
    hctx.stop().unwrap();

    assert_eq!(counters.drained.load(Ordering::Acquire), 1);
    assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
    assert_eq!(observer.completed.load(Ordering::Acquire), 1);
    assert_eq!(observer.failed.load(Ordering::Acquire), 1);
}
