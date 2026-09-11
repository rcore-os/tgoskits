use alloc::{
    sync::{Arc, Weak},
    task::Wake,
};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
    time::Duration,
};
use std::sync::mpsc;

use rdif_block::{
    BlkError, BlockController, ControllerEvent, ControllerState, ControllerUpdate, DriverGeneric,
    OwnedRequest, RequestFlags, RequestOp,
};

use super::{flush_barrier::barrier_test_inner, *};

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    future.poll(&mut context)
}

fn poll_with_waker<F: Future>(future: Pin<&mut F>, waker: &Waker) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(waker))
}

#[derive(Default)]
struct WakeCounter(AtomicUsize);

impl WakeCounter {
    fn count(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}

impl Wake for WakeCounter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

struct ReleaseOnDrop(Option<mpsc::Sender<()>>);

impl ReleaseOnDrop {
    fn release(&mut self) {
        if let Some(release) = self.0.take() {
            release.send(()).expect("the blocked worker remains alive");
        }
    }
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}

struct LifecycleLockCheckingWake {
    device: Weak<DeviceInner>,
    wakes: AtomicUsize,
    lock_failures: AtomicUsize,
}

impl LifecycleLockCheckingWake {
    fn observe(&self) {
        let unlocked = self
            .device
            .upgrade()
            .is_some_and(|device| device.lifecycle_gate.try_lock().is_some());
        if !unlocked {
            self.lock_failures.fetch_add(1, Ordering::AcqRel);
        }
        self.wakes.fetch_add(1, Ordering::AcqRel);
    }
}

impl Wake for LifecycleLockCheckingWake {
    fn wake(self: Arc<Self>) {
        self.observe();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.observe();
    }
}

struct ChannelLockCheckingWake {
    channel: Weak<BoundedChannel<Submission>>,
    wakes: AtomicUsize,
    lock_failures: AtomicUsize,
}

impl ChannelLockCheckingWake {
    fn observe(&self) {
        let unlocked = self
            .channel
            .upgrade()
            .is_some_and(|channel| channel.state_is_unlocked());
        if !unlocked {
            self.lock_failures.fetch_add(1, Ordering::AcqRel);
        }
        self.wakes.fetch_add(1, Ordering::AcqRel);
    }
}

impl Wake for ChannelLockCheckingWake {
    fn wake(self: Arc<Self>) {
        self.observe();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.observe();
    }
}

struct AsyncSubmissionController {
    queue: Option<LifecycleQueue>,
}

impl DriverGeneric for AsyncSubmissionController {
    fn name(&self) -> &str {
        "async-submission-controller"
    }
}

impl BlockController for AsyncSubmissionController {
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
                vec![Box::new(
                    self.queue.take().expect("async test queue is single-use"),
                )],
                Vec::new(),
            )),
            ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                Ok(ControllerUpdate::state(ControllerState::Shutdown))
            }
            _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
        }
    }
}

fn async_submission_handle() -> Arc<BlockDeviceHandle> {
    BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "async-submission-test",
        Vec::<BlockIrqSource>::new(),
        Box::new(AsyncSubmissionController {
            queue: Some(LifecycleQueue {
                log: Arc::new(StdMutex::new(Vec::new())),
            }),
        }),
    ))
    .expect("async submission test controller starts")
}

fn flush_request(flags: RequestFlags) -> OwnedRequest {
    OwnedRequest {
        op: RequestOp::Flush,
        lba: 0,
        block_count: 0,
        data: None,
        flags,
    }
}

fn read_request(lba: u64, info: QueueInfo) -> OwnedRequest {
    OwnedRequest {
        op: RequestOp::Read,
        lba,
        block_count: 1,
        data: Some(crate::block::runtime::dma::prepare_read(info.limits, 512).unwrap()),
        flags: RequestFlags::NONE,
    }
}

fn submission(request: OwnedRequest) -> Submission {
    let (_group, mut senders) = CompletionGroup::pairs(1).unwrap();
    Submission {
        request,
        completion: senders
            .pop_front()
            .expect("single test submission has one sender"),
    }
}

fn fill_channel_with_flush(channel: &BoundedChannel<Submission>) {
    assert!(matches!(
        channel.try_enqueue_no_notify(submission(flush_request(RequestFlags::NONE))),
        Ok(0)
    ));
}

#[test]
fn async_submit_returns_owned_request_when_no_cpu_channel_is_available() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let cpu_channels = core::mem::take(&mut *handle.inner.cpu_channels.lock());
    assert!(!cpu_channels.is_empty());

    let request = flush_request(RequestFlags::NONE);
    let mut future = Box::pin(handle.submit_owned_async(request));
    match poll_once(future.as_mut()) {
        Poll::Ready(Err(error)) => {
            assert_eq!(error.error, BlkError::Io);
            assert_eq!(error.into_request().op, RequestOp::Flush);
        }
        Poll::Ready(Ok(_)) => panic!("a device without a CPU channel accepted a request"),
        Poll::Pending => panic!("missing CPU channel must fail without waiting"),
    }

    drop(future);
    *handle.inner.cpu_channels.lock() = cpu_channels;
    assert_eq!(handle.shutdown(), 0);
}

#[test]
fn data_admission_waits_on_flush_and_rolls_back_when_cancelled() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().flush_active = true;

    let mut future = Box::pin(inner.acquire_data_async(RequestOp::Read, 1, false));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    assert_eq!(inner.lifecycle_gate.lock().active_data, 0);

    drop(future);
    let gate = inner.lifecycle_gate.lock();
    assert_eq!(gate.active_data, 0);
    assert!(gate.flush_active);
}

#[test]
fn flush_admission_holds_and_rolls_back_while_draining_data() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().active_data = 1;

    let mut future = Box::pin(inner.acquire_flush_async(false));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    assert!(inner.lifecycle_gate.lock().flush_active);

    drop(future);
    let gate = inner.lifecycle_gate.lock();
    assert_eq!(gate.active_data, 1);
    assert!(!gate.flush_active);
}

#[test]
fn flush_drain_notification_between_check_and_wait_is_observed() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().active_data = 1;
    let publisher = Arc::clone(&inner);
    inner.set_admission_wait_hook(move || {
        publisher.request_completed(RequestOp::Read, 1, Ok(()));
    });

    let mut future = Box::pin(inner.acquire_flush_async(false));
    let permit = match poll_once(future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        Poll::Pending => panic!("data-drain notification was lost before awaiting the listener"),
        Poll::Ready(Err(error)) => panic!("flush admission failed unexpectedly: {error:?}"),
    };
    let gate = inner.lifecycle_gate.lock();
    assert_eq!(gate.active_data, 0);
    assert!(gate.flush_active);
    drop(gate);
    drop(permit);
}

#[test]
fn second_flush_cancel_and_nowait_preserve_gate_owner() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    let mut owner_future = Box::pin(inner.acquire_flush_async(false));
    let owner = match poll_once(owner_future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        _ => panic!("the first flush should own an idle gate"),
    };
    drop(owner_future);

    let mut waiting = Box::pin(inner.acquire_flush_async(false));
    assert!(matches!(poll_once(waiting.as_mut()), Poll::Pending));
    drop(waiting);
    assert!(inner.lifecycle_gate.lock().flush_active);

    let mut nowait = Box::pin(inner.acquire_flush_async(true));
    assert!(matches!(
        poll_once(nowait.as_mut()),
        Poll::Ready(Err(BlkError::Retry))
    ));
    drop(nowait);
    assert!(inner.lifecycle_gate.lock().flush_active);

    drop(owner);
    assert!(!inner.lifecycle_gate.lock().flush_active);
}

#[test]
fn nowait_admission_returns_retry_without_pending() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().flush_active = true;

    let mut future = Box::pin(inner.acquire_data_async(RequestOp::Write, 1, true));
    assert!(matches!(
        poll_once(future.as_mut()),
        Poll::Ready(Err(BlkError::Retry))
    ));
    assert_eq!(inner.lifecycle_gate.lock().active_data, 0);
}

#[test]
fn ordinary_admission_future_remains_pending_in_nonblocking_context() {
    crate::os::task::install_test_runtime_ops();
    let _can_block = crate::os::task::test_can_block(false);
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().flush_active = true;

    let mut future = Box::pin(inner.acquire_data_async(RequestOp::Read, 1, false));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    assert_eq!(inner.lifecycle_gate.lock().active_data, 0);

    drop(future);
    assert!(inner.lifecycle_gate.lock().flush_active);
}

#[test]
fn admission_state_published_before_first_listener_check_is_observed() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();

    let mut future = Box::pin(inner.acquire_data_async(RequestOp::Read, 1, false));
    let permit = match poll_once(future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        Poll::Pending => panic!("admission should be available immediately"),
        Poll::Ready(Err(_)) => panic!("admission should succeed immediately"),
    };
    assert_eq!(inner.lifecycle_gate.lock().active_data, 1);
    drop(permit);
    drop(future);
}

#[test]
fn admission_notification_after_pending_poll_rechecks_the_gate() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().flush_active = true;

    let mut future = Box::pin(inner.acquire_data_async(RequestOp::Read, 1, false));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));

    inner.lifecycle_gate.lock().flush_active = false;
    inner.admission_async_waiters.notify_all();
    let permit = match poll_once(future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        Poll::Pending => panic!("notified admission remained pending"),
        Poll::Ready(Err(_)) => panic!("notified admission returned an error"),
    };
    drop(permit);
    drop(future);
}

#[test]
fn permit_release_wakes_sync_and_async_admission_waiters() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    let mut owner_future = Box::pin(inner.acquire_flush_async(false));
    let owner = match poll_once(owner_future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        _ => panic!("the flush should own an idle gate"),
    };
    drop(owner_future);

    let counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&counter));
    let mut async_waiter = Box::pin(inner.acquire_data_async(RequestOp::Read, 1, false));
    assert!(matches!(
        poll_with_waker(async_waiter.as_mut(), &waker),
        Poll::Pending
    ));

    let (registered_tx, registered_rx) = mpsc::channel();
    inner
        .data_gate_waiters
        .set_registration_hook(move || registered_tx.send(()).unwrap());
    let sync_device = Arc::clone(&inner);
    let (result_tx, result_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let sync_waiter = std::thread::spawn(move || {
        let result = sync_device.enter_data_submissions(1, SubmissionAdmission::Blocking);
        let admitted = result.is_ok();
        result_tx.send(result).unwrap();
        release_rx.recv().unwrap();
        if admitted {
            sync_device.undo_submission_admission(RequestOp::Read, 1);
        }
    });
    registered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("synchronous admission waiter did not register");

    drop(owner);
    assert_eq!(counter.count(), 1);
    assert_eq!(
        result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("synchronous admission waiter was not woken"),
        Ok(())
    );
    let async_permit = match poll_with_waker(async_waiter.as_mut(), &waker) {
        Poll::Ready(Ok(permit)) => permit,
        _ => panic!("asynchronous admission waiter was not woken"),
    };
    assert_eq!(inner.lifecycle_gate.lock().active_data, 2);

    release_tx.send(()).unwrap();
    sync_waiter.join().unwrap();
    assert_eq!(inner.lifecycle_gate.lock().active_data, 1);
    drop(async_permit);
    assert_eq!(inner.lifecycle_gate.lock().active_data, 0);
}

#[test]
fn admission_publisher_invokes_waker_after_releasing_gate_lock() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    let mut owner_future = Box::pin(inner.acquire_flush_async(false));
    let owner = match poll_once(owner_future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        _ => panic!("the flush should own an idle gate"),
    };
    drop(owner_future);

    let wake = Arc::new(LifecycleLockCheckingWake {
        device: Arc::downgrade(&inner),
        wakes: AtomicUsize::new(0),
        lock_failures: AtomicUsize::new(0),
    });
    let waker = Waker::from(Arc::clone(&wake));
    let mut future = Box::pin(inner.acquire_data_async(RequestOp::Read, 1, false));
    assert!(matches!(
        poll_with_waker(future.as_mut(), &waker),
        Poll::Pending
    ));

    drop(owner);
    assert_eq!(wake.wakes.load(Ordering::Acquire), 1);
    assert_eq!(wake.lock_failures.load(Ordering::Acquire), 0);
    let permit = match poll_with_waker(future.as_mut(), &waker) {
        Poll::Ready(Ok(permit)) => permit,
        _ => panic!("admission future did not observe the published gate state"),
    };
    drop(permit);
}

#[test]
fn admission_notification_between_check_and_wait_is_observed() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().flush_active = true;
    let publisher = Arc::clone(&inner);
    inner.set_admission_wait_hook(move || {
        publisher.undo_submission_admission(RequestOp::Flush, 1);
    });

    let mut future = Box::pin(inner.acquire_data_async(RequestOp::Read, 1, false));
    let permit = match poll_once(future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        Poll::Pending => panic!("admission notification was lost before awaiting the listener"),
        Poll::Ready(Err(error)) => panic!("admission failed unexpectedly: {error:?}"),
    };
    let gate = inner.lifecycle_gate.lock();
    assert!(!gate.flush_active);
    assert_eq!(gate.active_data, 1);
    drop(gate);
    drop(permit);
}

#[test]
fn flush_gate_notification_between_check_and_wait_is_observed() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    let mut owner_future = Box::pin(inner.acquire_flush_async(false));
    let owner = match poll_once(owner_future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        _ => panic!("the first flush should own an idle gate"),
    };
    drop(owner_future);
    inner.set_admission_wait_hook(move || drop(owner));

    let mut future = Box::pin(inner.acquire_flush_async(false));
    let permit = match poll_once(future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        Poll::Pending => panic!("flush gate notification was lost before awaiting the listener"),
        Poll::Ready(Err(error)) => panic!("flush admission failed unexpectedly: {error:?}"),
    };
    assert!(inner.lifecycle_gate.lock().flush_active);
    drop(permit);
}

#[test]
fn channel_async_waiter_is_woken_after_capacity_is_released() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let channel = original.hctx.new_submission_channel().unwrap();
    fill_channel_with_flush(&channel);
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&channel),
    };
    let release = Arc::clone(&channel);
    channel.set_space_wait_hook(move || {
        drop(
            release
                .try_recv()
                .expect("the test placeholder still occupies the channel"),
        );
    });

    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NONE)));
    let subscription = match poll_once(future.as_mut()) {
        Poll::Ready(Ok(subscription)) => subscription,
        Poll::Pending => panic!("capacity notification was lost before awaiting the listener"),
        Poll::Ready(Err(error)) => panic!("async submission failed: {:?}", error.error),
    };
    assert_eq!(channel.queued_len(), 1);
    drop(subscription);
    drop(future);
    let _ = handle.shutdown();
}

#[test]
fn channel_capacity_publisher_invokes_waker_after_releasing_state_lock() {
    crate::os::task::install_test_runtime_ops();
    let notification = runtime_ops().unwrap().notification();
    let channel = Arc::new(BoundedChannel::with_item_notification(1, notification).unwrap());
    fill_channel_with_flush(&channel);

    let wake = Arc::new(ChannelLockCheckingWake {
        channel: Arc::downgrade(&channel),
        wakes: AtomicUsize::new(0),
        lock_failures: AtomicUsize::new(0),
    });
    let waker = Waker::from(Arc::clone(&wake));
    let mut listener = Box::pin(channel.listen_for_space());
    assert!(matches!(
        poll_with_waker(listener.as_mut(), &waker),
        Poll::Pending
    ));

    drop(
        channel
            .try_recv()
            .expect("the test placeholder still occupies the channel"),
    );
    assert_eq!(wake.wakes.load(Ordering::Acquire), 1);
    assert_eq!(wake.lock_failures.load(Ordering::Acquire), 0);
    assert!(matches!(
        poll_with_waker(listener.as_mut(), &waker),
        Poll::Ready(())
    ));
}

#[test]
fn released_capacity_wakes_sync_and_async_channel_waiters() {
    crate::os::task::install_test_runtime_ops();
    let notification = runtime_ops().unwrap().notification();
    let channel = Arc::new(BoundedChannel::with_item_notification(1, notification).unwrap());
    fill_channel_with_flush(&channel);

    let counter = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&counter));
    let mut listener = Box::pin(channel.listen_for_space());
    assert!(matches!(
        poll_with_waker(listener.as_mut(), &waker),
        Poll::Pending
    ));

    let (registered_tx, registered_rx) = mpsc::channel();
    channel.set_capacity_registration_hook(move || registered_tx.send(()).unwrap());
    let sender_channel = Arc::clone(&channel);
    let (sent_tx, sent_rx) = mpsc::channel();
    let sender = std::thread::spawn(move || {
        let sent = sender_channel
            .send(submission(flush_request(RequestFlags::NONE)), false)
            .is_ok();
        sent_tx.send(sent).unwrap();
    });
    registered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("synchronous capacity waiter did not register");

    drop(
        channel
            .try_recv()
            .expect("the test placeholder still occupies the channel"),
    );
    assert_eq!(counter.count(), 1);
    assert!(matches!(
        poll_with_waker(listener.as_mut(), &waker),
        Poll::Ready(())
    ));
    assert!(
        sent_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("synchronous capacity waiter was not woken")
    );
    sender.join().unwrap();
    assert_eq!(channel.queued_len(), 1);
    drop(channel.try_recv());
}

#[test]
fn async_submit_repoll_replaces_channel_capacity_waker() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let replacement = original.hctx.new_submission_channel().unwrap();
    fill_channel_with_flush(&replacement);
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&replacement),
    };

    let first = Arc::new(WakeCounter::default());
    let first_waker = Waker::from(Arc::clone(&first));
    let second = Arc::new(WakeCounter::default());
    let second_waker = Waker::from(Arc::clone(&second));
    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NONE)));
    assert!(matches!(
        poll_with_waker(future.as_mut(), &first_waker),
        Poll::Pending
    ));
    assert!(matches!(
        poll_with_waker(future.as_mut(), &second_waker),
        Poll::Pending
    ));

    drop(
        replacement
            .try_recv()
            .expect("the test placeholder still occupies the channel"),
    );
    assert_eq!(first.count(), 0);
    assert_eq!(second.count(), 1);
    let subscription = match poll_with_waker(future.as_mut(), &second_waker) {
        Poll::Ready(Ok(subscription)) => subscription,
        Poll::Ready(Err(error)) => panic!("async submission failed: {:?}", error.error),
        Poll::Pending => panic!("notified submit future remained pending"),
    };
    drop(subscription);
    drop(future);
    let _ = handle.shutdown();
}

#[test]
fn ordinary_channel_wait_remains_pending_in_nonblocking_context() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let channel = original.hctx.new_submission_channel().unwrap();
    fill_channel_with_flush(&channel);
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&channel),
    };

    let can_block = crate::os::task::test_can_block(false);
    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NONE)));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    assert!(handle.inner.lifecycle_gate.lock().flush_active);
    assert_eq!(channel.queued_len(), 1);
    drop(future);
    assert!(!handle.inner.lifecycle_gate.lock().flush_active);
    drop(can_block);
    let _ = handle.shutdown();
}

#[test]
fn channel_close_wakes_every_async_capacity_waiter() {
    crate::os::task::install_test_runtime_ops();
    let notification = runtime_ops().unwrap().notification();
    let channel = BoundedChannel::with_item_notification(1, notification).unwrap();
    assert!(matches!(channel.try_enqueue_no_notify(1), Ok(0)));

    let mut first = Box::pin(channel.listen_for_space());
    let mut second = Box::pin(channel.listen_for_space());
    assert!(matches!(poll_once(first.as_mut()), Poll::Pending));
    assert!(matches!(poll_once(second.as_mut()), Poll::Pending));

    channel.close();
    assert!(matches!(poll_once(first.as_mut()), Poll::Ready(())));
    assert!(matches!(poll_once(second.as_mut()), Poll::Ready(())));
}

#[test]
fn async_flush_nowait_rolls_back_gate_after_data_drain_rejection() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().active_data = 1;

    let mut future = Box::pin(inner.acquire_flush_async(true));
    assert!(matches!(
        poll_once(future.as_mut()),
        Poll::Ready(Err(BlkError::Retry))
    ));
    let gate = inner.lifecycle_gate.lock();
    assert_eq!(gate.active_data, 1);
    assert!(!gate.flush_active);
}

#[test]
fn async_admission_returns_io_after_teardown_begins() {
    crate::os::task::install_test_runtime_ops();
    let inner = barrier_test_inner();
    inner.lifecycle_gate.lock().phase = DevicePhase::Stopping;

    let mut future = Box::pin(inner.acquire_data_async(RequestOp::Read, 1, false));
    assert!(matches!(
        poll_once(future.as_mut()),
        Poll::Ready(Err(BlkError::Io))
    ));
}

#[test]
fn async_data_submit_waiting_on_flush_gate_returns_dma_when_device_fails() {
    crate::os::task::install_test_runtime_ops();
    install_dma_op(&TEST_DMA_OP);
    let handle = async_submission_handle();
    let mut owner_future = Box::pin(handle.inner.acquire_flush_async(false));
    let owner = match poll_once(owner_future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        _ => panic!("the flush should own an idle gate"),
    };
    drop(owner_future);

    let wake = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wake));
    let mut future = Box::pin(handle.submit_owned_async(read_request(0, test_queue_info())));
    assert!(matches!(
        poll_with_waker(future.as_mut(), &waker),
        Poll::Pending
    ));
    assert_eq!(handle.inner.lifecycle_gate.lock().active_data, 0);

    handle.inner.mark_failed();
    assert_eq!(wake.count(), 1);
    let error = match poll_with_waker(future.as_mut(), &waker) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("a failed device admitted the pending data submission"),
        Poll::Pending => panic!("device failure did not wake the data admission waiter"),
    };
    assert_eq!(error.error, BlkError::Io);
    let returned = error.into_request();
    assert_eq!(returned.op, RequestOp::Read);
    assert!(returned.data.is_some());
    drop(crate::block::runtime::dma::complete_without_submit(
        returned.data,
    ));
    assert_eq!(handle.inner.lifecycle_gate.lock().active_data, 0);

    drop(future);
    drop(owner);
    assert!(!handle.inner.lifecycle_gate.lock().flush_active);
    let _ = handle.shutdown();
}

#[test]
fn async_flush_submit_waiting_on_data_drain_rolls_back_when_device_fails() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let mut owner_future = Box::pin(handle.inner.acquire_data_async(RequestOp::Read, 1, false));
    let owner = match poll_once(owner_future.as_mut()) {
        Poll::Ready(Ok(permit)) => permit,
        _ => panic!("the data request should own an idle gate"),
    };
    drop(owner_future);

    let wake = Arc::new(WakeCounter::default());
    let waker = Waker::from(Arc::clone(&wake));
    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NONE)));
    assert!(matches!(
        poll_with_waker(future.as_mut(), &waker),
        Poll::Pending
    ));
    {
        let gate = handle.inner.lifecycle_gate.lock();
        assert_eq!(gate.active_data, 1);
        assert!(gate.flush_active);
    }

    handle.inner.mark_failed();
    assert_eq!(wake.count(), 1);
    let error = match poll_with_waker(future.as_mut(), &waker) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("a failed device admitted the pending flush submission"),
        Poll::Pending => panic!("device failure did not wake the flush drain waiter"),
    };
    assert_eq!(error.error, BlkError::Io);
    assert_eq!(error.into_request().op, RequestOp::Flush);
    {
        let gate = handle.inner.lifecycle_gate.lock();
        assert_eq!(gate.active_data, 1);
        assert!(!gate.flush_active);
    }

    drop(future);
    drop(owner);
    assert_eq!(handle.inner.lifecycle_gate.lock().active_data, 0);
    let _ = handle.shutdown();
}

#[test]
fn async_submit_waits_for_full_channel_and_rolls_back_on_cancel() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let replacement = original.hctx.new_submission_channel().unwrap();
    fill_channel_with_flush(&replacement);
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&replacement),
    };

    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NONE)));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    assert!(handle.inner.lifecycle_gate.lock().flush_active);
    drop(future);
    assert!(!handle.inner.lifecycle_gate.lock().flush_active);
    assert_eq!(replacement.queued_len(), 1);
    let _ = handle.shutdown();
}

#[test]
fn async_data_submit_waits_for_full_channel_and_rolls_back_on_cancel() {
    crate::os::task::install_test_runtime_ops();
    install_dma_op(&TEST_DMA_OP);
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let replacement = original.hctx.new_submission_channel().unwrap();
    fill_channel_with_flush(&replacement);
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&replacement),
    };

    let mut future = Box::pin(handle.submit_owned_async(read_request(0, test_queue_info())));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    assert_eq!(handle.inner.lifecycle_gate.lock().active_data, 1);
    assert_eq!(replacement.queued_len(), 1);

    drop(future);
    assert_eq!(handle.inner.lifecycle_gate.lock().active_data, 0);
    assert_eq!(replacement.queued_len(), 1);
    let _ = handle.shutdown();
}

#[test]
fn async_submit_full_channel_returns_retry_when_channel_retires() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let replacement = original.hctx.new_submission_channel().unwrap();
    fill_channel_with_flush(&replacement);
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&replacement),
    };

    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NONE)));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    replacement.close();

    let error = match poll_once(future.as_mut()) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("a retired channel accepted a submission"),
        Poll::Pending => panic!("channel retirement did not wake the submit future"),
    };
    assert_eq!(error.error, BlkError::Retry);
    assert_eq!(error.into_request().op, RequestOp::Flush);
    assert!(!handle.inner.lifecycle_gate.lock().flush_active);
    assert_eq!(replacement.queued_len(), 1);
    drop(future);
    let _ = handle.shutdown();
}

#[test]
fn async_submit_waiting_on_full_channel_returns_io_when_device_fails() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let replacement = original.hctx.new_submission_channel().unwrap();
    fill_channel_with_flush(&replacement);
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&replacement),
    };

    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NONE)));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    assert!(handle.inner.lifecycle_gate.lock().flush_active);

    handle.inner.mark_failed();
    let error = match poll_once(future.as_mut()) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("a failed device accepted a pending submission"),
        Poll::Pending => panic!("device failure did not wake the pending submit future"),
    };
    assert_eq!(error.error, BlkError::Io);
    assert_eq!(error.into_request().op, RequestOp::Flush);
    assert!(!handle.inner.lifecycle_gate.lock().flush_active);
    assert_eq!(replacement.queued_len(), 1);

    drop(future);
    let _ = handle.shutdown();
}

#[test]
fn async_data_submit_waiting_on_full_channel_returns_dma_when_device_fails() {
    crate::os::task::install_test_runtime_ops();
    install_dma_op(&TEST_DMA_OP);
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let replacement = original.hctx.new_submission_channel().unwrap();
    fill_channel_with_flush(&replacement);
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&replacement),
    };

    let mut future = Box::pin(handle.submit_owned_async(read_request(0, test_queue_info())));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    assert_eq!(handle.inner.lifecycle_gate.lock().active_data, 1);

    handle.inner.mark_failed();
    let error = match poll_once(future.as_mut()) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("a failed device accepted a pending data submission"),
        Poll::Pending => panic!("device failure did not wake the pending data submission"),
    };
    assert_eq!(error.error, BlkError::Io);
    let returned = error.into_request();
    assert_eq!(returned.op, RequestOp::Read);
    assert!(returned.data.is_some());
    drop(crate::block::runtime::dma::complete_without_submit(
        returned.data,
    ));
    assert_eq!(handle.inner.lifecycle_gate.lock().active_data, 0);
    assert_eq!(replacement.queued_len(), 1);

    drop(future);
    let _ = handle.shutdown();
}

#[test]
fn async_submit_nowait_returns_request_when_channel_is_full() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let replacement = original.hctx.new_submission_channel().unwrap();
    fill_channel_with_flush(&replacement);
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&replacement),
    };

    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NOWAIT)));
    let error = match poll_once(future.as_mut()) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("NOWAIT request entered a full channel"),
        Poll::Pending => panic!("NOWAIT request waited on a full channel"),
    };
    assert_eq!(error.error, BlkError::Retry);
    assert_eq!(error.into_request().op, RequestOp::Flush);
    assert!(!handle.inner.lifecycle_gate.lock().flush_active);
    assert_eq!(replacement.queued_len(), 1);
    drop(future);
    let _ = handle.shutdown();
}

#[test]
fn async_submit_nowait_never_waits_on_channel_owner() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let replacement = original.hctx.new_submission_channel().unwrap();
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&replacement),
    };

    let (owner_ready_tx, owner_ready_rx) = mpsc::channel();
    let (release_owner_tx, release_owner_rx) = mpsc::channel();
    let owner_channel = Arc::clone(&replacement);
    let owner = std::thread::spawn(move || {
        owner_channel.with_state_lock_held(|| {
            owner_ready_tx.send(()).unwrap();
            release_owner_rx.recv().unwrap();
        });
    });
    owner_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("channel owner did not acquire the state lock");

    let contender_handle = Arc::clone(&handle);
    let (poll_started_tx, poll_started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let contender = std::thread::spawn(move || {
        let mut future =
            Box::pin(contender_handle.submit_owned_async(flush_request(RequestFlags::NOWAIT)));
        poll_started_tx.send(()).unwrap();
        let result = match poll_once(future.as_mut()) {
            Poll::Ready(Err(error)) => Ok((error.error, error.into_request().op)),
            Poll::Ready(Ok(subscription)) => {
                drop(subscription);
                Err("NOWAIT request entered a channel owned by another context")
            }
            Poll::Pending => Err("NOWAIT request returned Pending"),
        };
        result_tx.send(result).unwrap();
    });
    poll_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("NOWAIT contender did not start polling");
    let result = result_rx.recv_timeout(Duration::from_secs(1));

    release_owner_tx
        .send(())
        .expect("channel owner remains blocked on the release signal");
    owner.join().unwrap();
    contender.join().unwrap();

    assert_eq!(
        result.expect("NOWAIT submission blocked on the channel owner"),
        Ok((BlkError::Retry, RequestOp::Flush))
    );
    assert!(!handle.inner.lifecycle_gate.lock().flush_active);
    assert_eq!(replacement.queued_len(), 0);
    let _ = handle.shutdown();
}

#[test]
fn dropping_async_completion_receiver_keeps_enqueued_submission() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    let replacement = original.hctx.new_submission_channel().unwrap();
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: Arc::clone(&replacement),
    };

    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NONE)));
    let subscription = match poll_once(future.as_mut()) {
        Poll::Ready(Ok(subscription)) => subscription,
        Poll::Ready(Err(error)) => panic!("flush submission failed: {:?}", error.error),
        Poll::Pending => panic!("an empty test channel did not accept the flush"),
    };
    assert_eq!(replacement.queued_len(), 1);
    assert!(handle.inner.lifecycle_gate.lock().flush_active);
    drop(subscription);
    assert_eq!(replacement.queued_len(), 1);
    assert!(handle.inner.lifecycle_gate.lock().flush_active);

    drop(future);
    let _ = handle.shutdown();
}

#[test]
fn async_submit_keeps_sticky_channel_after_mapping_replacement() {
    crate::os::task::install_test_runtime_ops();
    let handle = async_submission_handle();
    let original = handle.inner.cpu_channels.lock()[0].clone();
    handle.inner.lifecycle_gate.lock().flush_active = true;

    let mut future = Box::pin(handle.submit_owned_async(flush_request(RequestFlags::NONE)));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));

    let replacement = original.hctx.add_submission_channel().unwrap();
    handle.inner.cpu_channels.lock()[0] = CpuSubmissionChannel {
        hctx: Arc::clone(&original.hctx),
        channel: replacement,
    };
    original.channel.close();
    handle.inner.lifecycle_gate.lock().flush_active = false;
    handle.inner.admission_async_waiters.notify_all();

    match poll_once(future.as_mut()) {
        Poll::Ready(Err(error)) => assert_eq!(error.error, BlkError::Retry),
        Poll::Ready(Ok(_)) => panic!("future re-selected a replacement channel"),
        Poll::Pending => panic!("sticky channel future remained pending after admission wake"),
    }
    drop(future);
    let _ = handle.shutdown();
}

#[test]
fn async_validation_error_returns_prepared_dma_to_the_caller() {
    crate::os::task::install_test_runtime_ops();
    install_dma_op(&TEST_DMA_OP);
    let handle = async_submission_handle();
    let info = test_queue_info();
    let request = read_request(info.device.num_blocks, info);

    let mut future = Box::pin(handle.submit_owned_async(request));
    let error = match poll_once(future.as_mut()) {
        Poll::Ready(Err(error)) => error,
        Poll::Ready(Ok(_)) => panic!("an out-of-range request was accepted"),
        Poll::Pending => panic!("request validation unexpectedly waited"),
    };
    assert_eq!(
        error.error,
        BlkError::InvalidBlockIndex(info.device.num_blocks)
    );
    let returned = error.into_request();
    assert!(returned.data.is_some());
    drop(crate::block::runtime::dma::complete_without_submit(
        returned.data,
    ));
    drop(future);
    assert_eq!(handle.shutdown(), 0);
}

#[test]
fn async_reads_respect_single_hardware_inflight_slot_and_return_dma() {
    let _registrar_guard = lock_test_irq_registrar();
    crate::os::task::install_test_runtime_ops();
    install_dma_op(&TEST_DMA_OP);
    let log = Arc::new(StdMutex::new(Vec::new()));
    configure_test_irq_registrar(log);

    let mut reported_info = batching_queue_info();
    reported_info.limits.max_inflight = 1;
    reported_info.limits.max_submit_batch = 1;
    let counters = Arc::new(BatchingQueueCounters::default());
    let (event_tx, event_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let controller = BatchingReadController {
        queue: Some(BatchingReadQueue {
            counters: Arc::clone(&counters),
            reported_info,
            next_id: 0,
            pending: Vec::new(),
            fail_next_drain: false,
            probe: Some(BatchingQueueProbe {
                events: event_tx,
                release_first_submission: Some(release_rx),
            }),
        }),
    };
    let irq = IrqId::new(IrqDomainId(1), HwIrq(18));
    let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
        "async-single-inflight",
        [BlockIrqSource { source_id: 0, irq }],
        Box::new(controller),
    ))
    .unwrap();
    let mut release_first = ReleaseOnDrop(Some(release_tx));
    let submission_channel = Arc::clone(&handle.inner.cpu_channels.lock()[0].channel);

    let mut first_submit = Box::pin(handle.submit_owned_async(read_request(0, reported_info)));
    let first_subscription = match poll_once(first_submit.as_mut()) {
        Poll::Ready(Ok(subscription)) => subscription,
        Poll::Ready(Err(error)) => panic!("first read submission failed: {:?}", error.error),
        Poll::Pending => panic!("empty submission channel rejected the first read"),
    };
    drop(first_submit);
    assert_eq!(
        event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("hardware queue did not accept the first read"),
        BatchingQueueEvent::Accepted {
            count: 1,
            pending: 1,
        }
    );

    let mut second_submit = Box::pin(handle.submit_owned_async(read_request(1, reported_info)));
    let second_subscription = match poll_once(second_submit.as_mut()) {
        Poll::Ready(Ok(subscription)) => subscription,
        Poll::Ready(Err(error)) => panic!("second read submission failed: {:?}", error.error),
        Poll::Pending => panic!("the released software channel did not accept the second read"),
    };
    drop(second_submit);
    assert_eq!(submission_channel.queued_len(), 1);

    let first_wake = Arc::new(WakeCounter::default());
    let first_waker = Waker::from(Arc::clone(&first_wake));
    let mut first_completion = Box::pin(first_subscription.recv_async());
    assert!(matches!(
        poll_with_waker(first_completion.as_mut(), &first_waker),
        Poll::Pending
    ));
    let second_wake = Arc::new(WakeCounter::default());
    let second_waker = Waker::from(Arc::clone(&second_wake));
    let mut second_completion = Box::pin(second_subscription.recv_async());
    assert!(matches!(
        poll_with_waker(second_completion.as_mut(), &second_waker),
        Poll::Pending
    ));

    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    release_first.release();
    assert_eq!(
        event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the first latched IRQ did not drain the first read"),
        BatchingQueueEvent::Drained { count: 1 }
    );
    let first = match poll_with_waker(first_completion.as_mut(), &first_waker) {
        Poll::Ready(completed) => completed,
        Poll::Pending => panic!("the first read completion was not published"),
    };
    assert_eq!(first.result, Ok(()));
    assert!(first.data.is_some());
    assert_eq!(first.data.as_ref().unwrap().len().get(), 512);
    assert_eq!(second_wake.count(), 0);
    assert!(matches!(
        poll_with_waker(second_completion.as_mut(), &second_waker),
        Poll::Pending
    ));
    assert_eq!(
        event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the second read was not submitted after the first completion"),
        BatchingQueueEvent::Accepted {
            count: 1,
            pending: 1,
        }
    );
    assert_eq!(submission_channel.queued_len(), 0);

    assert_eq!(
        TEST_IRQ_REGISTRAR.run_registered_action(),
        BlockIrqOutcome::Wake
    );
    assert_eq!(
        event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the second IRQ did not drain the second read"),
        BatchingQueueEvent::Drained { count: 1 }
    );
    let second = match poll_with_waker(second_completion.as_mut(), &second_waker) {
        Poll::Ready(completed) => completed,
        Poll::Pending => panic!("the second read completion was not published"),
    };
    assert_eq!(second.result, Ok(()));
    assert!(second.data.is_some());
    assert_eq!(second.data.as_ref().unwrap().len().get(), 512);
    assert_ne!(first.id, second.id);
    assert_eq!(counters.submitted.load(Ordering::Acquire), 2);
    assert_eq!(counters.peak_pending.load(Ordering::Acquire), 1);
    drop(first);
    drop(second);
    assert_eq!(handle.shutdown(), 1);
}
