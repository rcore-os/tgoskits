use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::{
    future::poll_fn,
    sync::atomic::{AtomicUsize, Ordering},
    task::Poll,
};

use atomic_waker::AtomicWaker;
use rdif_block::{BlkError, CompletedRequest};

use crate::os::{BlockNotification, runtime_ops, sync::IrqMutex};

/// One-shot receiver for one owned block request.
pub struct CompletionSubscription {
    cell: Arc<CompletionCell>,
}

/// Blocking receivers for an ordered group of owned block requests.
pub struct CompletionGroup {
    subscriptions: Vec<CompletionSubscription>,
}

pub(super) struct CompletionSender {
    cell: Arc<CompletionCell>,
}

struct CompletionCell {
    state: IrqMutex<CompletionState>,
    waker: AtomicWaker,
    group: Arc<CompletionBarrier>,
    #[cfg(test)]
    poll_hook: IrqMutex<Option<alloc::boxed::Box<dyn FnOnce() + Send>>>,
}

struct CompletionBarrier {
    remaining: AtomicUsize,
    notification: Arc<dyn BlockNotification>,
}

struct CompletionState {
    result: Option<CompletedRequest>,
    receiver_alive: bool,
}

impl CompletionSubscription {
    #[cfg(test)]
    pub(super) fn pair() -> Result<(Self, CompletionSender), BlkError> {
        let notification = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .notification();
        Ok(Self::pair_with_notification(notification))
    }

    #[cfg(test)]
    fn pair_with_notification(
        notification: Arc<dyn BlockNotification>,
    ) -> (Self, CompletionSender) {
        let group = Arc::new(CompletionBarrier {
            remaining: AtomicUsize::new(1),
            notification,
        });
        Self::pair_with_group(group)
    }

    fn pair_with_group(group: Arc<CompletionBarrier>) -> (Self, CompletionSender) {
        let cell = Arc::new(CompletionCell {
            state: IrqMutex::new(CompletionState {
                result: None,
                receiver_alive: true,
            }),
            waker: AtomicWaker::new(),
            group,
            #[cfg(test)]
            poll_hook: IrqMutex::new(None),
        });
        (
            Self {
                cell: Arc::clone(&cell),
            },
            CompletionSender { cell },
        )
    }

    /// Blocks until the maintenance task publishes a terminal completion.
    ///
    /// Use [`Self::recv_async`] to suspend the current task without blocking.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime adapter is unavailable or the current
    /// context is not allowed to sleep.
    pub fn recv(self) -> Result<CompletedRequest, BlkError> {
        let ops =
            runtime_ops().map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
        if !ops.can_block() {
            return Err(BlkError::Other(
                "block completion receive requires a sleepable task",
            ));
        }
        loop {
            let result = {
                let mut state = self.cell.state.lock();
                let result = state.result.take();
                if result.is_some() {
                    state.receiver_alive = false;
                }
                result
            };
            if let Some(result) = result {
                return Ok(result);
            }
            self.cell.group.notification.wait();
        }
    }

    /// Asynchronously waits for the maintenance task to publish a terminal
    /// completion.
    ///
    /// This future may only be polled or dropped from task context or another
    /// context where deferred work is allowed. Dropping it discards the
    /// receiver but does not cancel an I/O that has already been submitted.
    pub async fn recv_async(self) -> CompletedRequest {
        poll_fn(|context| {
            self.cell.waker.register(context.waker());
            #[cfg(test)]
            self.cell.run_poll_hook();
            let result = {
                let mut state = self.cell.state.lock();
                let result = state.result.take();
                if result.is_some() {
                    state.receiver_alive = false;
                }
                result
            };
            if let Some(result) = result {
                drop(self.cell.waker.take());
                Poll::Ready(result)
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

#[cfg(test)]
impl CompletionCell {
    fn set_poll_hook(&self, hook: impl FnOnce() + Send + 'static) {
        let previous = self.poll_hook.lock().replace(alloc::boxed::Box::new(hook));
        assert!(previous.is_none(), "completion poll hook already installed");
    }

    fn run_poll_hook(&self) {
        let hook = self.poll_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl CompletionGroup {
    pub(super) fn pairs(count: usize) -> Result<(Self, VecDeque<CompletionSender>), BlkError> {
        if count == 0 {
            return Err(BlkError::InvalidRequest);
        }
        let notification = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .notification();
        Self::pairs_with_notification(count, notification)
    }

    fn pairs_with_notification(
        count: usize,
        notification: Arc<dyn BlockNotification>,
    ) -> Result<(Self, VecDeque<CompletionSender>), BlkError> {
        let mut subscriptions = Vec::new();
        subscriptions
            .try_reserve_exact(count)
            .map_err(|_| BlkError::NoMemory)?;
        let mut senders = VecDeque::new();
        senders
            .try_reserve_exact(count)
            .map_err(|_| BlkError::NoMemory)?;
        let barrier = Arc::new(CompletionBarrier {
            remaining: AtomicUsize::new(count),
            notification,
        });
        for _ in 0..count {
            let (subscription, sender) =
                CompletionSubscription::pair_with_group(Arc::clone(&barrier));
            subscriptions.push(subscription);
            senders.push_back(sender);
        }
        Ok((Self { subscriptions }, senders))
    }

    pub(super) fn into_single(mut self) -> Result<CompletionSubscription, BlkError> {
        if self.subscriptions.len() != 1 {
            return Err(BlkError::InvalidRequest);
        }
        self.subscriptions.pop().ok_or(BlkError::InvalidRequest)
    }

    /// Returns the number of completion subscriptions in this group.
    pub fn len(&self) -> usize {
        self.subscriptions.len()
    }

    /// Returns whether this group contains no subscriptions.
    pub fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }

    /// Blocks until every request has completed and returns results in
    /// submission order.
    ///
    /// Hardware completion order may differ. No polling or nonblocking receive
    /// API is provided.
    ///
    /// # Errors
    ///
    /// Returns an error if the current context cannot sleep or the runtime
    /// adapter is unavailable.
    pub fn recv(self) -> Result<Vec<CompletedRequest>, BlkError> {
        let mut completed = Vec::new();
        completed
            .try_reserve_exact(self.subscriptions.len())
            .map_err(|_| BlkError::NoMemory)?;
        for subscription in self.subscriptions {
            completed.push(subscription.recv()?);
        }
        Ok(completed)
    }
}

impl Drop for CompletionSubscription {
    fn drop(&mut self) {
        let result = {
            let mut state = self.cell.state.lock();
            state.receiver_alive = false;
            state.result.take()
        };
        drop(self.cell.waker.take());
        drop(result);
    }
}

impl CompletionSender {
    pub(super) fn complete(self, request: CompletedRequest) {
        let unclaimed = {
            let mut state = self.cell.state.lock();
            if state.receiver_alive {
                state.result = Some(request);
                None
            } else {
                Some(request)
            }
        };
        drop(unclaimed);

        let last_group_member = self.cell.group.remaining.fetch_sub(1, Ordering::AcqRel) == 1;
        self.cell.waker.wake();
        // The AcqRel countdown makes the final publisher the single blocking
        // group notification owner. Wake the one-shot receiver first so both
        // completion paths observe the published result in the same order.
        if last_group_member {
            self.cell.group.notification.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{
        boxed::Box,
        sync::{Arc, Weak},
        task::Wake,
    };
    use core::{
        alloc::Layout,
        future::Future,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll, Waker},
        time::Duration,
    };
    use std::{
        alloc::{alloc_zeroed, dealloc},
        sync::{Barrier, mpsc},
        thread,
    };

    use dma_api::{
        CpuDmaBuffer, DeviceDma, DmaAddr, DmaAllocHandle, DmaCoherency, DmaConstraints,
        DmaDeviceInfo, DmaDirection, DmaDomainId, DmaError, DmaMapHandle, DmaOp,
    };
    use rdif_block::{BlkError, CompletedRequest, RequestId};

    use super::{
        BlockNotification, CompletionCell, CompletionGroup, CompletionSender,
        CompletionSubscription,
    };

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl WakeCounter {
        fn count(&self) -> usize {
            self.0.load(Ordering::Acquire)
        }

        fn bump(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.bump();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.bump();
        }
    }

    struct LockCheckingWake {
        cell: Weak<CompletionCell>,
        wakes: AtomicUsize,
        lock_failures: AtomicUsize,
        publication_failures: AtomicUsize,
    }

    impl LockCheckingWake {
        fn run(&self) {
            let Some(cell) = self.cell.upgrade() else {
                self.lock_failures.fetch_add(1, Ordering::AcqRel);
                self.wakes.fetch_add(1, Ordering::AcqRel);
                return;
            };
            let Some(state) = cell.state.try_lock() else {
                self.lock_failures.fetch_add(1, Ordering::AcqRel);
                self.wakes.fetch_add(1, Ordering::AcqRel);
                return;
            };
            if state.result.is_none() {
                self.publication_failures.fetch_add(1, Ordering::AcqRel);
            }
            self.wakes.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl Wake for LockCheckingWake {
        fn wake(self: Arc<Self>) {
            self.run();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.run();
        }
    }

    #[derive(Default)]
    struct DropObservation {
        drops: AtomicUsize,
        lock_failures: AtomicUsize,
        wakes: AtomicUsize,
    }

    struct DropCheckingWake {
        cell: Weak<CompletionCell>,
        observation: Arc<DropObservation>,
    }

    impl Wake for DropCheckingWake {
        fn wake(self: Arc<Self>) {
            self.observation.wakes.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl Drop for DropCheckingWake {
        fn drop(&mut self) {
            let lock_available = self
                .cell
                .upgrade()
                .is_some_and(|cell| cell.state.try_lock().is_some());
            if !lock_available {
                self.observation
                    .lock_failures
                    .fetch_add(1, Ordering::AcqRel);
            }
            self.observation.drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct BlockingWake {
        entered: mpsc::Sender<()>,
        release: std::sync::Mutex<mpsc::Receiver<()>>,
        wakes: Arc<AtomicUsize>,
    }

    impl BlockingWake {
        fn run(&self) {
            self.wakes.fetch_add(1, Ordering::AcqRel);
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
        }
    }

    impl Wake for BlockingWake {
        fn wake(self: Arc<Self>) {
            self.run();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.run();
        }
    }

    struct DmaDropProbe {
        cell: Weak<CompletionCell>,
        observation: Arc<DropObservation>,
    }

    impl DmaOp for DmaDropProbe {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            // SAFETY: `layout` comes from dma-api and describes a nonzero byte
            // allocation that this backend releases with the same layout.
            let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
            // SAFETY: both CPU and allocation addresses name the live
            // allocation above, which remains owned until deallocation.
            Some(unsafe {
                DmaAllocHandle::new(ptr, ptr, DmaAddr::from(ptr.as_ptr() as u64), layout)
            })
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            let lock_available = self
                .cell
                .upgrade()
                .is_some_and(|cell| cell.state.try_lock().is_some());
            if !lock_available {
                self.observation
                    .lock_failures
                    .fetch_add(1, Ordering::AcqRel);
            }
            self.observation.drops.fetch_add(1, Ordering::AcqRel);
            // SAFETY: dma-api returns exactly one handle produced by
            // `alloc_contiguous`, with its original allocation pointer/layout.
            unsafe { dealloc(handle.allocation_ptr().as_ptr(), handle.layout()) };
        }

        unsafe fn alloc_coherent(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            // SAFETY: this test backend uses the same allocator and ownership
            // contract for coherent and ordinary contiguous allocations.
            unsafe { self.alloc_contiguous(constraints, layout) }
        }

        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
            // SAFETY: `handle` came from this backend's coherent allocator.
            unsafe { self.dealloc_contiguous(handle) };
            Ok(())
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            Err(DmaError::NoMemory)
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    fn pair() -> (CompletionSubscription, CompletionSender) {
        let notification: Arc<dyn BlockNotification> = Arc::new(CountingNotification::default());
        CompletionSubscription::pair_with_notification(notification)
    }

    fn completed(id: usize) -> CompletedRequest {
        CompletedRequest::new(RequestId::new(id), Ok(()), None)
    }

    fn completed_with_dma(
        id: usize,
        cell: Weak<CompletionCell>,
        observation: Arc<DropObservation>,
    ) -> CompletedRequest {
        let dma_op = Box::leak(Box::new(DmaDropProbe { cell, observation }));
        let device = DeviceDma::new(
            DmaDeviceInfo::new(
                DmaDomainId::Direct,
                DmaCoherency::Coherent,
                DmaConstraints::new(u64::MAX),
            ),
            dma_op,
        );
        let data = CpuDmaBuffer::new_zero(
            &device,
            NonZeroUsize::new(512).unwrap(),
            1,
            DmaDirection::ToDevice,
        )
        .unwrap()
        .prepare_for_device()
        .complete_without_device();
        CompletedRequest::new(RequestId::new(id), Ok(()), Some(data))
    }

    #[derive(Default)]
    struct CountingNotification {
        notifications: AtomicUsize,
    }

    impl BlockNotification for CountingNotification {
        fn notify(&self) {
            self.notifications.fetch_add(1, Ordering::Relaxed);
        }

        #[track_caller]
        fn wait(&self) {
            unreachable!("the completion publisher test does not block")
        }

        #[track_caller]
        fn wait_timeout(&self, _duration: Duration) -> bool {
            unreachable!("the completion publisher test does not block")
        }
    }

    #[test]
    fn completion_group_notifies_waiter_once_after_all_members_complete() {
        let notification = Arc::new(CountingNotification::default());
        let notification_dyn: Arc<dyn BlockNotification> = notification.clone();
        let (_group, senders) =
            CompletionGroup::pairs_with_notification(4, notification_dyn).unwrap();

        for (index, sender) in senders.into_iter().enumerate() {
            sender.complete(CompletedRequest::new(RequestId::new(index), Ok(()), None));
        }

        assert_eq!(notification.notifications.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn completion_future_delivers_published_result() {
        let (subscription, sender) = pair();
        let mut future = Box::pin(subscription.recv_async());
        let mut context = Context::from_waker(Waker::noop());

        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        sender.complete(completed(7));

        let Poll::Ready(completed) = future.as_mut().poll(&mut context) else {
            panic!("completion future remained pending after publication");
        };
        assert_eq!(completed.id, RequestId::new(7));
        assert_eq!(completed.result, Ok(()));
    }

    #[test]
    fn async_completion_composes_in_nonblocking_context_where_sync_receive_rejects() {
        crate::os::task::install_test_runtime_ops();
        let _can_block = crate::os::task::test_can_block(false);

        let (sync_subscription, _sync_sender) = pair();
        assert!(matches!(
            sync_subscription.recv(),
            Err(BlkError::Other(
                "block completion receive requires a sleepable task"
            ))
        ));

        let (subscription, sender) = pair();
        let async_progress = Arc::new(AtomicUsize::new(0));
        let mut completion = Box::pin(subscription.recv_async());
        let progress_count = Arc::clone(&async_progress);
        let mut progress = Box::pin(core::future::poll_fn(move |_| {
            progress_count.fetch_add(1, Ordering::AcqRel);
            Poll::Ready(())
        }));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            completion.as_mut().poll(&mut context),
            Poll::Pending
        ));

        assert!(matches!(
            progress.as_mut().poll(&mut context),
            Poll::Ready(())
        ));
        assert_eq!(async_progress.load(Ordering::Acquire), 1);
        sender.complete(completed(7));
        assert!(matches!(
            completion.as_mut().poll(&mut context),
            Poll::Ready(completed) if completed.id == RequestId::new(7)
        ));
    }

    #[test]
    fn completion_before_first_poll_is_observed() {
        let (subscription, sender) = pair();
        sender.complete(completed(11));
        let mut future = Box::pin(subscription.recv_async());
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(counter.clone());
        let mut context = Context::from_waker(&waker);

        let Poll::Ready(completed) = future.as_mut().poll(&mut context) else {
            panic!("completion published before registration was lost");
        };
        assert_eq!(completed.id, RequestId::new(11));
        assert_eq!(counter.count(), 0);
    }

    #[test]
    fn completion_between_waker_registration_and_result_recheck_is_observed() {
        let (subscription, sender) = pair();
        subscription
            .cell
            .set_poll_hook(move || sender.complete(completed(13)));
        let mut future = Box::pin(subscription.recv_async());
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(counter.clone());
        let mut context = Context::from_waker(&waker);

        let Poll::Ready(completed) = future.as_mut().poll(&mut context) else {
            panic!("completion in the register/recheck interval was lost");
        };
        assert_eq!(completed.id, RequestId::new(13));
        assert_eq!(counter.count(), 1);
    }

    #[test]
    fn repeated_poll_replaces_the_registered_waker() {
        let (subscription, sender) = pair();
        let mut future = Box::pin(subscription.recv_async());
        let first = Arc::new(WakeCounter::default());
        let first_waker = Waker::from(first.clone());
        let mut first_context = Context::from_waker(&first_waker);
        let second = Arc::new(WakeCounter::default());
        let second_waker = Waker::from(second.clone());
        let mut second_context = Context::from_waker(&second_waker);

        assert!(matches!(
            future.as_mut().poll(&mut first_context),
            Poll::Pending
        ));
        assert!(matches!(
            future.as_mut().poll(&mut second_context),
            Poll::Pending
        ));
        sender.complete(completed(17));

        assert_eq!(first.count(), 0);
        assert_eq!(second.count(), 1);
        assert!(matches!(
            future.as_mut().poll(&mut second_context),
            Poll::Ready(completed) if completed.id == RequestId::new(17)
        ));
    }

    #[test]
    fn non_final_group_member_wakes_its_own_future() {
        let notification = Arc::new(CountingNotification::default());
        let notification_dyn: Arc<dyn BlockNotification> = notification.clone();
        let (group, mut senders) =
            CompletionGroup::pairs_with_notification(2, notification_dyn).unwrap();
        let mut subscriptions = group.subscriptions.into_iter();
        let first_subscription = subscriptions.next().unwrap();
        let second_subscription = subscriptions.next().unwrap();
        let first_sender = senders.pop_front().unwrap();
        let second_sender = senders.pop_front().unwrap();
        let mut first_future = Box::pin(first_subscription.recv_async());
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(counter.clone());
        let mut context = Context::from_waker(&waker);

        assert!(matches!(
            first_future.as_mut().poll(&mut context),
            Poll::Pending
        ));
        first_sender.complete(completed(19));

        assert_eq!(counter.count(), 1);
        assert_eq!(notification.notifications.load(Ordering::Acquire), 0);
        assert!(matches!(
            first_future.as_mut().poll(&mut context),
            Poll::Ready(completed) if completed.id == RequestId::new(19)
        ));

        second_sender.complete(completed(23));
        assert_eq!(notification.notifications.load(Ordering::Acquire), 1);
        drop(second_subscription);
    }

    #[test]
    fn publisher_invokes_waker_after_releasing_the_cell_lock() {
        let (subscription, sender) = pair();
        let cell = Arc::downgrade(&subscription.cell);
        let wake = Arc::new(LockCheckingWake {
            cell,
            wakes: AtomicUsize::new(0),
            lock_failures: AtomicUsize::new(0),
            publication_failures: AtomicUsize::new(0),
        });
        let waker = Waker::from(wake.clone());
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(subscription.recv_async());

        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        sender.complete(completed(29));

        assert_eq!(wake.wakes.load(Ordering::Acquire), 1);
        assert_eq!(wake.lock_failures.load(Ordering::Acquire), 0);
        assert_eq!(wake.publication_failures.load(Ordering::Acquire), 0);
    }

    #[test]
    fn dropping_pending_future_clears_waker_outside_lock_after_sender_drop() {
        let (subscription, sender) = pair();
        let cell = Arc::downgrade(&subscription.cell);
        let observation = Arc::new(DropObservation::default());
        let waker = Waker::from(Arc::new(DropCheckingWake {
            cell: cell.clone(),
            observation: observation.clone(),
        }));
        let mut future = Box::pin(subscription.recv_async());

        {
            let mut context = Context::from_waker(&waker);
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        }
        drop(waker);
        assert_eq!(observation.drops.load(Ordering::Acquire), 0);

        drop(sender);
        assert!(cell.upgrade().is_some());
        drop(future);

        assert_eq!(observation.drops.load(Ordering::Acquire), 1);
        assert_eq!(observation.lock_failures.load(Ordering::Acquire), 0);
        assert!(cell.upgrade().is_none());
    }

    #[test]
    fn receiver_cancel_and_publisher_release_dma_once_outside_the_cell_lock() {
        let (subscription, sender) = pair();
        let observation = Arc::new(DropObservation::default());
        let request =
            completed_with_dma(31, Arc::downgrade(&subscription.cell), observation.clone());
        let start = Arc::new(Barrier::new(3));
        let receiver_start = start.clone();
        let receiver = thread::spawn(move || {
            receiver_start.wait();
            drop(subscription);
        });
        let publisher_start = start.clone();
        let publisher = thread::spawn(move || {
            publisher_start.wait();
            sender.complete(request);
        });

        start.wait();
        receiver.join().unwrap();
        publisher.join().unwrap();

        assert_eq!(observation.drops.load(Ordering::Acquire), 1);
        assert_eq!(observation.lock_failures.load(Ordering::Acquire), 0);
    }

    #[test]
    fn stale_wake_can_finish_after_the_future_is_dropped() {
        let (subscription, sender) = pair();
        let mut future = Box::pin(subscription.recv_async());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let wakes = Arc::new(AtomicUsize::new(0));
        let waker = Waker::from(Arc::new(BlockingWake {
            entered: entered_tx,
            release: std::sync::Mutex::new(release_rx),
            wakes: wakes.clone(),
        }));

        {
            let mut context = Context::from_waker(&waker);
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        }
        drop(waker);
        let publisher = thread::spawn(move || sender.complete(completed(37)));
        entered_rx
            .recv()
            .expect("publisher did not invoke the registered waker");

        drop(future);
        release_tx.send(()).unwrap();
        publisher.join().unwrap();

        assert_eq!(wakes.load(Ordering::Acquire), 1);
    }
}
