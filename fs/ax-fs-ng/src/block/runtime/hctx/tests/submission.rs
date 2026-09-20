use alloc::{sync::Weak, task::Wake};
use core::{
    future::Future,
    task::{Context, Poll, Waker},
};

use super::*;

struct SubmissionRegistryLockCheckingWake {
    state: Weak<HctxState>,
    wakes: AtomicUsize,
    lock_failures: AtomicUsize,
}

impl SubmissionRegistryLockCheckingWake {
    fn observe(&self) {
        let unlocked = self
            .state
            .upgrade()
            .is_some_and(|state| state.submission_channels.try_lock().is_some());
        if !unlocked {
            self.lock_failures.fetch_add(1, Ordering::AcqRel);
        }
        self.wakes.fetch_add(1, Ordering::AcqRel);
    }
}

impl Wake for SubmissionRegistryLockCheckingWake {
    fn wake(self: Arc<Self>) {
        self.observe();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.observe();
    }
}

#[test]
fn closing_channels_checks_state_after_releasing_submission_registry_lock() {
    crate::os::task::install_test_runtime_ops();
    let channel = Arc::new(
        BoundedChannel::with_item_notification(1, runtime_ops().unwrap().notification()).unwrap(),
    );
    let state = Arc::new(HctxState::test_new(
        test_queue_info(1),
        vec![Arc::clone(&channel)],
    ));
    let hctx = Hctx {
        id: 0,
        cpu: 0,
        state,
        thread: IrqMutex::new(None),
    };

    hctx.close_submission_channels();

    assert!(channel.is_closed());
}

#[test]
fn prune_checks_channel_state_after_releasing_submission_registry_lock() {
    crate::os::task::install_test_runtime_ops();
    let channel = Arc::new(
        BoundedChannel::with_item_notification(1, runtime_ops().unwrap().notification()).unwrap(),
    );
    channel.close();
    let state = HctxState::test_new(test_queue_info(1), vec![channel]);

    prune_closed_submission_channels(&state);

    assert!(state.submission_channels.lock().is_empty());
}

#[test]
fn dequeue_wakes_capacity_listener_after_releasing_submission_registry_lock() {
    crate::os::task::install_test_runtime_ops();
    let channel = Arc::new(
        BoundedChannel::with_item_notification(1, runtime_ops().unwrap().notification()).unwrap(),
    );
    let (subscription, queued) = flush_submission();
    assert!(channel.send(queued, true).is_ok());
    let state = Arc::new(HctxState::test_new(
        test_queue_info(1),
        vec![Arc::clone(&channel)],
    ));
    let wake = Arc::new(SubmissionRegistryLockCheckingWake {
        state: Arc::downgrade(&state),
        wakes: AtomicUsize::new(0),
        lock_failures: AtomicUsize::new(0),
    });
    let waker = Waker::from(Arc::clone(&wake));
    let mut listener = Box::pin(channel.listen_for_space());
    assert!(matches!(
        listener.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Pending
    ));

    let mut retry_submissions = VecDeque::new();
    let mut next_channel = 0;
    let mut prefer_retry = false;
    let mut submissions = VecDeque::new();
    collect_submission_batch(
        &state,
        &mut retry_submissions,
        &mut next_channel,
        &mut prefer_retry,
        1,
        &mut submissions,
    );

    assert_eq!(submissions.len(), 1);
    assert_eq!(wake.wakes.load(Ordering::Acquire), 1);
    assert_eq!(wake.lock_failures.load(Ordering::Acquire), 0);
    assert!(matches!(
        listener.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Ready(())
    ));
    drop(submissions);
    drop(subscription);
}

#[test]
fn single_dequeue_wakes_after_releasing_submission_registry_lock() {
    crate::os::task::install_test_runtime_ops();
    let channel = Arc::new(
        BoundedChannel::with_item_notification(1, runtime_ops().unwrap().notification()).unwrap(),
    );
    let (subscription, queued) = flush_submission();
    assert!(channel.send(queued, true).is_ok());
    let state = Arc::new(HctxState::test_new(
        test_queue_info(1),
        vec![Arc::clone(&channel)],
    ));
    let wake = Arc::new(SubmissionRegistryLockCheckingWake {
        state: Arc::downgrade(&state),
        wakes: AtomicUsize::new(0),
        lock_failures: AtomicUsize::new(0),
    });
    let waker = Waker::from(Arc::clone(&wake));
    let mut listener = Box::pin(channel.listen_for_space());
    assert!(matches!(
        listener.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Pending
    ));
    let (retry_subscription, retry) = flush_submission();

    let mut retry_submissions = VecDeque::from([retry]);
    let mut next_channel = 0;
    let mut prefer_retry = false;
    let mut submissions = VecDeque::new();
    collect_submission_batch(
        &state,
        &mut retry_submissions,
        &mut next_channel,
        &mut prefer_retry,
        1,
        &mut submissions,
    );

    assert_eq!(submissions.len(), 1);
    assert_eq!(retry_submissions.len(), 1);
    assert_eq!(wake.wakes.load(Ordering::Acquire), 1);
    assert_eq!(wake.lock_failures.load(Ordering::Acquire), 0);
    assert!(matches!(
        listener.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Ready(())
    ));
    drop(submissions);
    drop(retry_submissions);
    drop(subscription);
    drop(retry_subscription);
}

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

    let mut action = queue_zero_action(&hctx);
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

    let mut action = queue_zero_action(&hctx);
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

    let mut action = queue_zero_action(&hctx);
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

    let mut action = queue_zero_action(&hctx);
    assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);

    assert_eq!(subscription.recv().unwrap().result, Err(BlkError::Io));
    hctx.stop().unwrap();

    assert_eq!(counters.drained.load(Ordering::Acquire), 1);
    assert_eq!(counters.shutdown.load(Ordering::Acquire), 1);
    assert_eq!(observer.completed.load(Ordering::Acquire), 1);
    assert_eq!(observer.failed.load(Ordering::Acquire), 1);
}
