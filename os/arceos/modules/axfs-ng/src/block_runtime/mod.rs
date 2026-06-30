pub use crate::block::runtime::*;

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
    use core::{
        any::Any,
        cell::Cell,
        sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    };
    use std::{
        collections::HashMap,
        sync::{Arc as StdArc, Condvar, Mutex as StdMutex, OnceLock, mpsc},
    };

    use ax_errno::AxError;
    use dma_api::{DeviceDma, DmaDomainId};
    use rdif_block::{
        BlkError, CompletionHint, DeviceInfo, DriverGeneric, IQueue, IQueueOwned, Interface,
        OwnedRequest, PollError, QueueHandle, QueueInfo, QueueLimits, Request, RequestId,
        RequestOp, RequestPoll, RequestStatus, SubmitError,
    };

    use super::*;
    use crate::os::{BlockTaskOps, install_dma_op, set_task_ops, sync::IrqMutex as SpinNoIrq};

    static TEST_TASK_OPS: TestTaskOps = TestTaskOps;
    static NEXT_TEST_TASK_ID: AtomicU64 = AtomicU64::new(1_000_000);
    static TEST_TIMEOUT_WAITS: AtomicUsize = AtomicUsize::new(0);
    static TEST_TASKS: OnceLock<StdMutex<HashMap<u64, StdArc<TestTaskState>>>> = OnceLock::new();
    static TEST_TASK_LOCK: StdMutex<()> = StdMutex::new(());

    thread_local! {
        static TEST_TASK_ID: Cell<u64> = const { Cell::new(0) };
        static TEST_TASK_BLOCKING: Cell<bool> = const { Cell::new(false) };
    }

    struct TestTaskOps;

    struct TestTaskState {
        ready: StdMutex<bool>,
        cvar: Condvar,
    }

    impl TestTaskState {
        fn new() -> Self {
            Self {
                ready: StdMutex::new(false),
                cvar: Condvar::new(),
            }
        }
    }

    impl BlockTaskOps for TestTaskOps {
        fn current_task_id(&self) -> Option<u64> {
            test_task_is_blocking().then(current_test_task_id)
        }

        fn task_yield(&self) {
            std::thread::yield_now();
        }

        fn task_wait(&self) {
            if !test_task_is_blocking() {
                std::thread::yield_now();
                return;
            }
            let state = current_test_task_state();
            let mut ready = state.ready.lock().unwrap();
            while !*ready {
                ready = state.cvar.wait(ready).unwrap();
            }
            *ready = false;
        }

        fn task_wait_timeout(&self, dur: core::time::Duration) -> bool {
            if !test_task_is_blocking() {
                std::thread::yield_now();
                return true;
            }
            let state = current_test_task_state();
            let mut ready = state.ready.lock().unwrap();
            if !*ready {
                let (next_ready, timeout) = state.cvar.wait_timeout(ready, dur).unwrap();
                ready = next_ready;
                if timeout.timed_out() {
                    TEST_TIMEOUT_WAITS.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
            }
            *ready = false;
            false
        }

        fn task_wait_until(&self, condition: &dyn Fn() -> bool) {
            if !test_task_is_blocking() {
                while !condition() {
                    std::thread::yield_now();
                }
                return;
            }
            let state = current_test_task_state();
            let mut ready = state.ready.lock().unwrap();
            while !condition() {
                while !*ready {
                    ready = state.cvar.wait(ready).unwrap();
                }
                *ready = false;
            }
        }

        fn wake_task(&self, task_id: u64) {
            let Some(state) = test_tasks().lock().unwrap().get(&task_id).cloned() else {
                return;
            };
            let mut ready = state.ready.lock().unwrap();
            *ready = true;
            state.cvar.notify_one();
        }

        fn notify_waiters(&self) {
            for state in test_tasks().lock().unwrap().values() {
                let mut ready = state.ready.lock().unwrap();
                *ready = true;
                state.cvar.notify_all();
            }
        }

        fn notify_drain(&self) {
            self.notify_waiters();
        }

        fn notify_drain_from_irq(&self) {
            self.notify_drain();
        }

        fn wait_for_drain_notification(&self) {
            self.task_wait();
        }
    }

    fn install_test_task_ops() {
        set_task_ops(&TEST_TASK_OPS);
        install_dma_op(&VEC_DMA_OP);
    }

    fn with_blocking_task<R>(f: impl FnOnce() -> R) -> R {
        install_test_task_ops();
        TEST_TASK_BLOCKING.with(|blocking| blocking.set(true));
        let task_id = current_test_task_id();
        let _guard = TestTaskGuard { task_id };
        f()
    }

    fn test_task_guard() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_TASK_LOCK.lock().unwrap();
        clear_test_tasks();
        guard
    }

    struct TestTaskGuard {
        task_id: u64,
    }

    impl Drop for TestTaskGuard {
        fn drop(&mut self) {
            TEST_TASK_BLOCKING.with(|blocking| blocking.set(false));
            TEST_TASK_ID.with(|id| id.set(0));
            test_tasks().lock().unwrap().remove(&self.task_id);
        }
    }

    fn test_task_is_blocking() -> bool {
        TEST_TASK_BLOCKING.with(Cell::get)
    }

    fn current_test_task_id() -> u64 {
        TEST_TASK_ID.with(|id| {
            let existing = id.get();
            if existing != 0 {
                return existing;
            }
            let new_id = NEXT_TEST_TASK_ID.fetch_add(1, Ordering::AcqRel);
            test_tasks()
                .lock()
                .unwrap()
                .insert(new_id, StdArc::new(TestTaskState::new()));
            id.set(new_id);
            new_id
        })
    }

    fn current_test_task_state() -> StdArc<TestTaskState> {
        let task_id = current_test_task_id();
        test_tasks()
            .lock()
            .unwrap()
            .get(&task_id)
            .cloned()
            .expect("blocking test task must be registered")
    }

    fn test_tasks() -> &'static StdMutex<HashMap<u64, StdArc<TestTaskState>>> {
        TEST_TASKS.get_or_init(|| StdMutex::new(HashMap::new()))
    }

    fn clear_test_tasks() {
        if let Some(tasks) = TEST_TASKS.get() {
            tasks.lock().unwrap().clear();
        }
    }

    struct ChannelDrainWake {
        tx: std::sync::Mutex<mpsc::Sender<()>>,
    }

    impl BlockDrainWake for ChannelDrainWake {
        fn wake_drain(&self) {
            let _ = self.tx.lock().unwrap().send(());
        }
    }

    fn noop_config() -> BlockRuntimeConfig {
        install_test_task_ops();
        BlockRuntimeConfig::new(Arc::new(NoopDrainWake))
    }

    fn channel_config(tx: mpsc::Sender<()>) -> BlockRuntimeConfig {
        install_test_task_ops();
        BlockRuntimeConfig::new(Arc::new(ChannelDrainWake {
            tx: std::sync::Mutex::new(tx),
        }))
    }

    fn irq_driven_config() -> BlockRuntimeConfig {
        let mut config = noop_config();
        config.completion_mode = BlockCompletionMode::IrqDriven;
        config
    }

    fn wait_for_pending_count(device: &BlockDeviceHandle, queue_id: usize, expected: usize) {
        for _ in 0..1000 {
            if device.pending_count_for_queue(queue_id) == expected {
                return;
            }
            std::thread::yield_now();
        }
        assert_eq!(device.pending_count_for_queue(queue_id), expected);
    }

    fn drain_queue_hint_until_complete(
        device: &BlockDeviceHandle,
        bridge: &BlockIrqBridge,
        queue_id: usize,
        expected: usize,
    ) -> usize {
        let initial_pending = device.pending_count_for_queue(queue_id);
        let mut completed = 0;
        for _ in 0..1000 {
            bridge.record_hint(CompletionHint::Queue { queue_id });
            completed += device.drain_events();
            let removed_by_racing_task_poll =
                initial_pending.saturating_sub(device.pending_count_for_queue(queue_id));
            if completed + removed_by_racing_task_poll >= expected {
                return expected;
            }
            std::thread::yield_now();
        }
        let removed_by_racing_task_poll =
            initial_pending.saturating_sub(device.pending_count_for_queue(queue_id));
        completed + removed_by_racing_task_poll
    }

    #[derive(Default)]
    struct Poller {
        completions: BTreeMap<DriverKey, Result<RequestStatus, BlkError>>,
        polled: Vec<DriverKey>,
    }

    impl Poller {
        fn complete(&mut self, key: DriverKey) {
            self.completions.insert(key, Ok(RequestStatus::Complete));
        }

        fn fail(&mut self, key: DriverKey) {
            self.completions.insert(key, Err(BlkError::Io));
        }
    }

    impl RequestPoller for Poller {
        fn poll_request(
            &mut self,
            queue_id: usize,
            request_id: RequestId,
        ) -> Result<PollOutcome, BlkError> {
            let key = (queue_id, request_id);
            self.polled.push(key);
            self.completions
                .remove(&key)
                .unwrap_or(Ok(RequestStatus::Pending))
                .map(poll_outcome_from_status)
        }
    }

    struct BatchOnlyPoller {
        completions: BTreeMap<DriverKey, Result<RequestStatus, BlkError>>,
        batch_errors_remaining: usize,
        batch_calls: usize,
        single_polls: usize,
        last_batch: Vec<DriverKey>,
    }

    impl BatchOnlyPoller {
        fn new(completions: BTreeMap<DriverKey, Result<RequestStatus, BlkError>>) -> Self {
            Self {
                completions,
                batch_errors_remaining: 0,
                batch_calls: 0,
                single_polls: 0,
                last_batch: Vec::new(),
            }
        }

        fn with_batch_errors(mut self, count: usize) -> Self {
            self.batch_errors_remaining = count;
            self
        }
    }

    impl RequestPoller for BatchOnlyPoller {
        fn poll_request(
            &mut self,
            _queue_id: usize,
            _request_id: RequestId,
        ) -> Result<PollOutcome, BlkError> {
            self.single_polls += 1;
            Ok(PollOutcome::Pending)
        }

        fn poll_completions(
            &mut self,
            queue_id: usize,
            request_ids: &[RequestId],
            sink: &mut dyn CompletionSink,
        ) -> Result<(), BlkError> {
            self.batch_calls += 1;
            self.last_batch = request_ids
                .iter()
                .map(|request_id| (queue_id, *request_id))
                .collect();
            if self.batch_errors_remaining > 0 {
                self.batch_errors_remaining -= 1;
                return Err(BlkError::Io);
            }

            for &request_id in request_ids {
                if let Some(result) = self.completions.remove(&(queue_id, request_id)) {
                    match result {
                        Ok(RequestStatus::Pending) => {}
                        Ok(RequestStatus::Complete) => {
                            sink.complete(request_id, Ok(()));
                        }
                        Err(err) => {
                            sink.complete(request_id, Err(err));
                        }
                    }
                }
            }
            Ok(())
        }
    }

    type DriverKey = (usize, RequestId);

    fn key(id: usize) -> RequestKey {
        RuntimeRequestId::new(id)
    }

    fn driver_key(id: usize) -> DriverKey {
        (0, RequestId::new(id))
    }

    #[test]
    fn request_completes_before_wait_token_registration() {
        let mut table = PendingTable::new();
        table.insert_submitted(0, RequestId::new(1), None).unwrap();
        assert!(table.complete(key(1), Ok(())).is_none());

        assert_eq!(table.register_waiter_task(key(1), 7), Some(Ok(())));
        assert_eq!(
            table.take_completed(key(1)).map(|(result, _)| result),
            Some(Ok(()))
        );
    }

    #[test]
    fn request_completes_after_waiter_task_registration() {
        let mut table = PendingTable::new();
        let key = table.insert_submitted(0, RequestId::new(2), None).unwrap();
        assert_eq!(table.register_waiter_task(key, 7), None);

        let wake = table.complete(key, Ok(())).unwrap();
        assert_eq!(wake, 7);
        assert_eq!(
            table.take_completed(key).map(|(result, _)| result),
            Some(Ok(()))
        );
    }

    #[test]
    fn runtime_request_key_survives_driver_request_id_reuse() {
        let mut table = PendingTable::new();
        let first = table.insert_submitted(0, RequestId::new(1), None).unwrap();
        assert!(table.complete(first, Ok(())).is_none());
        let second = table.insert_submitted(0, RequestId::new(1), None).unwrap();

        assert_ne!(first, second);
        assert_eq!(
            table.take_completed(first).map(|(result, _)| result),
            Some(Ok(()))
        );
        assert!(table.request(second).is_some());
        assert_eq!(table.result(second), None);
    }

    #[test]
    fn pending_table_rejects_inflight_driver_request_id_reuse() {
        let mut table = PendingTable::new();
        table.insert_submitted(0, RequestId::new(1), None).unwrap();

        assert_eq!(
            table.insert_submitted(0, RequestId::new(1), None),
            Err(BlkError::InvalidRequest)
        );
    }

    #[test]
    fn request_hint_wakes_only_matching_request() {
        let pending = SpinNoIrq::new(PendingTable::new());
        pending
            .lock()
            .insert_submitted(0, RequestId::new(1), None)
            .unwrap();
        pending
            .lock()
            .insert_submitted(0, RequestId::new(2), None)
            .unwrap();
        pending.lock().register_waiter_task(key(1), 1);
        pending.lock().register_waiter_task(key(2), 2);

        let mut poller = Poller::default();
        poller.complete(driver_key(1));
        let mut drain = CompletionDrain::new(&pending, &mut poller);
        drain.drain_hint(CompletionHint::Request {
            queue_id: 0,
            request_id: RequestId::new(1),
        });

        assert!(pending.lock().request(key(2)).is_some());
    }

    #[test]
    fn queue_hint_scans_all_pending_requests_on_queue() {
        let pending = SpinNoIrq::new(PendingTable::new());
        pending
            .lock()
            .insert_submitted(0, RequestId::new(1), None)
            .unwrap();
        pending
            .lock()
            .insert_submitted(0, RequestId::new(2), None)
            .unwrap();
        pending.lock().register_waiter_task(key(1), 1);
        pending.lock().register_waiter_task(key(2), 2);

        let mut poller = Poller::default();
        poller.complete(driver_key(1));
        poller.complete(driver_key(2));
        let mut drain = CompletionDrain::new(&pending, &mut poller);
        assert_eq!(drain.drain_hint(CompletionHint::Queue { queue_id: 0 }), 2);
    }

    #[test]
    fn queue_hint_uses_batch_completion_query_for_pending_requests() {
        let pending = SpinNoIrq::new(PendingTable::new());
        pending
            .lock()
            .insert_submitted(0, RequestId::new(1), None)
            .unwrap();
        pending
            .lock()
            .insert_submitted(0, RequestId::new(2), None)
            .unwrap();
        pending.lock().register_waiter_task(key(1), 1);
        pending.lock().register_waiter_task(key(2), 2);

        let mut completions = BTreeMap::new();
        completions.insert(driver_key(1), Ok(RequestStatus::Complete));
        completions.insert(driver_key(2), Ok(RequestStatus::Complete));
        let mut poller = BatchOnlyPoller::new(completions);
        let mut drain = CompletionDrain::new(&pending, &mut poller);

        assert_eq!(drain.drain_hint(CompletionHint::Queue { queue_id: 0 }), 2);
        assert_eq!(poller.batch_calls, 1);
        assert_eq!(poller.single_polls, 0);
        assert_eq!(poller.last_batch, [driver_key(1), driver_key(2)]);
    }

    #[test]
    fn batch_hint_uses_batch_completion_query_for_matching_requests() {
        let pending = SpinNoIrq::new(PendingTable::new());
        pending
            .lock()
            .insert_submitted(0, RequestId::new(1), None)
            .unwrap();
        pending
            .lock()
            .insert_submitted(0, RequestId::new(2), None)
            .unwrap();
        pending
            .lock()
            .insert_submitted(0, RequestId::new(3), None)
            .unwrap();
        pending.lock().register_waiter_task(key(1), 1);
        pending.lock().register_waiter_task(key(2), 2);
        pending.lock().register_waiter_task(key(3), 3);

        let mut completions = BTreeMap::new();
        completions.insert(driver_key(1), Ok(RequestStatus::Complete));
        completions.insert(driver_key(3), Ok(RequestStatus::Complete));
        let mut poller = BatchOnlyPoller::new(completions);
        let mut drain = CompletionDrain::new(&pending, &mut poller);

        let mut ids = rdif_block::CompletionIds::new();
        assert!(ids.push(RequestId::new(1)));
        assert!(ids.push(RequestId::new(3)));
        assert_eq!(
            drain.drain_hint(CompletionHint::Batch { queue_id: 0, ids }),
            2
        );
        assert_eq!(poller.batch_calls, 1);
        assert_eq!(poller.single_polls, 0);
        assert_eq!(poller.last_batch, [driver_key(1), driver_key(3)]);
    }

    #[test]
    fn batch_poll_interface_error_does_not_complete_claimed_requests() {
        let pending = SpinNoIrq::new(PendingTable::new());
        let key = pending
            .lock()
            .insert_submitted(0, RequestId::new(1), None)
            .unwrap();
        pending.lock().register_waiter_task(key, 4);

        let mut completions = BTreeMap::new();
        completions.insert(driver_key(1), Ok(RequestStatus::Complete));
        let mut poller = BatchOnlyPoller::new(completions).with_batch_errors(1);
        let mut drain = CompletionDrain::new(&pending, &mut poller);

        assert_eq!(drain.drain_hint(CompletionHint::Queue { queue_id: 0 }), 0);
        assert_eq!(pending.lock().result(key), None);

        assert_eq!(drain.drain_hint(CompletionHint::Queue { queue_id: 0 }), 1);
        assert_eq!(
            pending.lock().take_completed(key).map(|(result, _)| result),
            Some(Ok(()))
        );
    }

    #[test]
    fn drain_events_groups_mixed_queue_batches() {
        let pending = SpinNoIrq::new(PendingTable::new());
        pending
            .lock()
            .insert_submitted(0, RequestId::new(1), None)
            .unwrap();
        pending
            .lock()
            .insert_submitted(1, RequestId::new(7), None)
            .unwrap();
        pending.lock().register_waiter_task(key(1), 1);
        pending.lock().register_waiter_task(key(2), 2);

        let mut completions = BTreeMap::new();
        completions.insert((0, RequestId::new(1)), Ok(RequestStatus::Complete));
        completions.insert((1, RequestId::new(7)), Ok(RequestStatus::Complete));
        let mut poller = BatchOnlyPoller::new(completions);
        let mut drain = CompletionDrain::new(&pending, &mut poller);

        assert_eq!(
            drain.drain_events(DrainEvents {
                queue_bits: 0b11,
                hints: rdif_block::CompletionList::new(),
            }),
            2
        );
        assert_eq!(poller.batch_calls, 2);
        assert_eq!(poller.single_polls, 0);
    }

    #[test]
    fn queue_hint_does_not_repoll_request_completed_by_submit_side_poll() {
        let pending = SpinNoIrq::new(PendingTable::new());
        pending
            .lock()
            .insert_submitted(0, RequestId::new(1), None)
            .unwrap();
        assert!(pending.lock().complete(key(1), Ok(())).is_none());

        let mut poller = Poller::default();
        poller.fail(driver_key(1));
        let mut drain = CompletionDrain::new(&pending, &mut poller);

        assert_eq!(drain.drain_hint(CompletionHint::Queue { queue_id: 0 }), 0);
        assert!(poller.polled.is_empty());
        assert_eq!(
            pending
                .lock()
                .take_completed(key(1))
                .map(|(result, _)| result),
            Some(Ok(()))
        );
    }

    #[test]
    fn request_hint_does_not_overwrite_existing_completion_result() {
        let pending = SpinNoIrq::new(PendingTable::new());
        pending
            .lock()
            .insert_submitted(0, RequestId::new(1), None)
            .unwrap();
        assert!(pending.lock().complete(key(1), Ok(())).is_none());

        let mut poller = Poller::default();
        poller.fail(driver_key(1));
        let mut drain = CompletionDrain::new(&pending, &mut poller);

        assert_eq!(
            drain.drain_hint(CompletionHint::Request {
                queue_id: 0,
                request_id: RequestId::new(1)
            }),
            0
        );
        assert!(poller.polled.is_empty());
        assert_eq!(
            pending
                .lock()
                .take_completed(key(1))
                .map(|(result, _)| result),
            Some(Ok(()))
        );
    }

    #[test]
    fn pending_table_allows_only_one_active_poll_per_request() {
        let mut table = PendingTable::new();
        table.insert_submitted(0, RequestId::new(1), None).unwrap();

        assert_eq!(table.begin_poll(key(1)), PollClaim::Claimed);
        assert_eq!(table.begin_poll(key(1)), PollClaim::AlreadyPolling);
        assert_eq!(table.finish_pending_poll(key(1)), PollProgress::Repoll);
        assert_eq!(table.finish_pending_poll(key(1)), PollProgress::Pending);
        assert_eq!(table.begin_poll(key(1)), PollClaim::Claimed);
    }

    #[test]
    fn completed_request_cannot_reenter_poll_after_submit_side_completion() {
        let mut table = PendingTable::new();
        table.insert_submitted(0, RequestId::new(1), None).unwrap();

        assert_eq!(table.begin_poll(key(1)), PollClaim::Claimed);
        assert!(table.complete(key(1), Ok(())).is_none());
        assert_eq!(table.begin_poll(key(1)), PollClaim::MissingOrComplete);
        assert_eq!(
            table.take_completed(key(1)).map(|(result, _)| result),
            Some(Ok(()))
        );
    }

    struct RepollPoller<'a> {
        pending: &'a SpinNoIrq<PendingTable>,
        key: RequestKey,
        polls: usize,
    }

    impl RequestPoller for RepollPoller<'_> {
        fn poll_request(
            &mut self,
            _queue_id: usize,
            _request_id: RequestId,
        ) -> Result<PollOutcome, BlkError> {
            self.polls += 1;
            if self.polls == 1 {
                assert_eq!(
                    self.pending.lock().begin_poll(self.key),
                    PollClaim::AlreadyPolling
                );
                Ok(PollOutcome::Pending)
            } else {
                Ok(PollOutcome::complete(Ok(())))
            }
        }
    }

    fn poll_outcome_from_status(status: RequestStatus) -> PollOutcome {
        match status {
            RequestStatus::Pending => PollOutcome::Pending,
            RequestStatus::Complete => PollOutcome::complete(Ok(())),
        }
    }

    #[test]
    fn completion_hint_during_active_poll_is_polled_again_before_sleep() {
        let pending = SpinNoIrq::new(PendingTable::new());
        let key = pending
            .lock()
            .insert_submitted(0, RequestId::new(1), None)
            .unwrap();
        pending.lock().register_waiter_task(key, 1);

        let mut poller = RepollPoller {
            pending: &pending,
            key,
            polls: 0,
        };
        let mut drain = CompletionDrain::new(&pending, &mut poller);
        assert_eq!(
            drain.drain_hint(CompletionHint::Request {
                queue_id: 0,
                request_id: RequestId::new(1),
            }),
            1
        );

        assert_eq!(poller.polls, 2);
        assert_eq!(
            pending.lock().take_completed(key).map(|(result, _)| result),
            Some(Ok(()))
        );
    }

    #[test]
    fn failed_request_wakes_waiter_with_error() {
        let pending = SpinNoIrq::new(PendingTable::new());
        let key = pending
            .lock()
            .insert_submitted(0, RequestId::new(3), None)
            .unwrap();
        pending.lock().register_waiter_task(key, 1);

        let mut poller = Poller::default();
        poller.fail(driver_key(3));
        let mut drain = CompletionDrain::new(&pending, &mut poller);
        drain.drain_hint(CompletionHint::Request {
            queue_id: 0,
            request_id: RequestId::new(3),
        });

        assert_eq!(
            pending.lock().take_completed(key).map(|(result, _)| result),
            Some(Err(BlkError::Io))
        );
    }

    #[test]
    fn abandoned_request_keeps_buffer_guard_until_completion_drain() {
        let pending = SpinNoIrq::new(PendingTable::new());
        let info = QueueInfo {
            id: 0,
            device: DeviceInfo::new(8, 512),
            limits: QueueLimits::simple(512, u64::MAX),
        };
        let planner = rdif_block::TransferPlanner::new(
            info.device,
            info.limits,
            rdif_block::TransferRuntimeCaps::new(512, 1),
        )
        .unwrap();
        let chunk = planner.plan(0, 512).unwrap().next().unwrap();
        let dma = DeviceDma::new(DmaDomainId::legacy_global(), u64::MAX, &VEC_DMA_OP);
        let guard =
            DmaBufferGuard::new(&dma, 512, 1, dma_api::DmaDirection::FromDevice, chunk, None)
                .unwrap();
        let key = pending
            .lock()
            .insert_submitted(0, RequestId::new(4), Some(RuntimeDmaBuffer::Legacy(guard)))
            .unwrap();
        pending.lock().abandon(key);
        assert_eq!(
            pending.lock().request(key).map(PendingRequest::state),
            Some(RequestState::Abandoned)
        );
        assert!(
            pending
                .lock()
                .request(key)
                .is_some_and(PendingRequest::holds_buffer_guard)
        );

        assert!(pending.lock().complete(key, Ok(())).is_none());
        assert!(pending.lock().request(key).is_some());
        assert!(
            !pending
                .lock()
                .request(key)
                .is_some_and(PendingRequest::holds_buffer_guard)
        );
        assert_eq!(
            pending.lock().take_completed(key).map(|(result, _)| result),
            Some(Ok(()))
        );
    }

    #[test]
    fn irq_bridge_records_request_hints_without_polling() {
        let bridge = BlockIrqBridge::new();
        bridge.record_hint(CompletionHint::Request {
            queue_id: 1,
            request_id: RequestId::new(9),
        });

        assert!(bridge.drain_ready());
        let events = bridge.take_events();
        assert_eq!(events.queue_bits, 0);
        assert_eq!(events.hints.len(), 1);
    }

    #[test]
    fn irq_bridge_keeps_ready_for_events_recorded_after_drain() {
        let bridge = BlockIrqBridge::new();
        bridge.record_hint(CompletionHint::Request {
            queue_id: 0,
            request_id: RequestId::new(1),
        });
        let events = bridge.take_events();
        assert_eq!(events.hints.len(), 1);
        assert!(!bridge.drain_ready());

        bridge.record_hint(CompletionHint::Request {
            queue_id: 0,
            request_id: RequestId::new(2),
        });
        assert!(bridge.drain_ready());
        let events = bridge.take_events();
        assert_eq!(events.hints.len(), 1);
    }

    struct MockQueue {
        info: QueueInfo,
        next: usize,
        storage: Vec<u8>,
        requests: BTreeMap<RequestId, MockRequest>,
        submitted: Arc<AtomicUsize>,
        submitted_requests: Arc<SpinNoIrq<Vec<SubmittedMockRequest>>>,
        fail_first_poll_lba: Option<u64>,
        pending_polls_before_complete: usize,
        retry_submits: usize,
        retry_while_pending: bool,
        reuse_request_id: Option<RequestId>,
        repoll_hook: Option<RepollHook>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct SubmittedMockRequest {
        queue_id: usize,
        op: RequestOp,
        lba: u64,
        block_count: u32,
    }

    struct MockRequest {
        lba: u64,
        pending_polls_remaining: usize,
    }

    struct RepollHook {
        polled: mpsc::Sender<RequestId>,
        resume: mpsc::Receiver<()>,
    }

    impl MockQueue {
        fn new() -> Self {
            Self::with_id(0)
        }

        fn with_id(id: usize) -> Self {
            Self {
                info: QueueInfo {
                    id,
                    device: DeviceInfo::new(16, 512),
                    limits: QueueLimits {
                        max_inflight: 8,
                        max_blocks_per_request: 2,
                        max_segment_size: 512,
                        ..QueueLimits::simple(512, u64::MAX)
                    },
                },
                next: 1,
                storage: (0..16 * 512).map(|idx| (idx / 512) as u8).collect(),
                requests: BTreeMap::new(),
                submitted: Arc::new(AtomicUsize::new(0)),
                submitted_requests: Arc::new(SpinNoIrq::new(Vec::new())),
                fail_first_poll_lba: None,
                pending_polls_before_complete: 1,
                retry_submits: 0,
                retry_while_pending: false,
                reuse_request_id: None,
                repoll_hook: None,
            }
        }

        fn with_submit_counter(id: usize, submitted: Arc<AtomicUsize>) -> Self {
            let mut queue = Self::with_id(id);
            queue.submitted = submitted;
            queue
        }

        fn with_request_log(id: usize, log: Arc<SpinNoIrq<Vec<SubmittedMockRequest>>>) -> Self {
            let mut queue = Self::with_id(id);
            queue.submitted_requests = log;
            queue
        }

        fn with_limits(id: usize, limits: QueueLimits) -> Self {
            let mut queue = Self::with_id(id);
            queue.info.limits = limits;
            queue
        }

        fn with_request_log_and_limits(
            id: usize,
            log: Arc<SpinNoIrq<Vec<SubmittedMockRequest>>>,
            limits: QueueLimits,
        ) -> Self {
            let mut queue = Self::with_request_log(id, log);
            queue.info.limits = limits;
            queue
        }

        fn with_first_poll_failure_on_lba(lba: u64) -> Self {
            let mut queue = Self::new();
            queue.fail_first_poll_lba = Some(lba);
            queue
        }

        fn with_pending_polls_before_complete(count: usize) -> Self {
            let mut queue = Self::new();
            queue.pending_polls_before_complete = count;
            queue
        }

        fn with_retry_submits(count: usize) -> Self {
            let mut queue = Self::new();
            queue.retry_submits = count;
            queue
        }

        fn with_retry_while_pending() -> Self {
            let mut queue = Self::with_pending_polls_before_complete(3);
            queue.info.limits.max_inflight = 1;
            queue.retry_while_pending = true;
            queue
        }

        fn with_reused_request_id(request_id: RequestId) -> Self {
            let mut queue = Self::with_pending_polls_before_complete(2);
            queue.reuse_request_id = Some(request_id);
            queue
        }

        fn with_repoll_hook(polled: mpsc::Sender<RequestId>, resume: mpsc::Receiver<()>) -> Self {
            let mut queue = Self::with_pending_polls_before_complete(1);
            queue.repoll_hook = Some(RepollHook { polled, resume });
            queue
        }
    }

    // SAFETY: The mock copies write data at submit time and only writes read
    // data into the request segment during poll completion.
    unsafe impl IQueue for MockQueue {
        fn id(&self) -> usize {
            self.info.id
        }

        fn info(&self) -> QueueInfo {
            self.info
        }

        fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
            if self.retry_submits > 0 {
                self.retry_submits -= 1;
                return Err(BlkError::Retry);
            }
            if self.retry_while_pending && !self.requests.is_empty() {
                return Err(BlkError::Retry);
            }
            self.submitted.fetch_add(1, Ordering::AcqRel);
            self.submitted_requests.lock().push(SubmittedMockRequest {
                queue_id: self.info.id,
                op: request.op,
                lba: request.lba,
                block_count: request.block_count,
            });
            let id = if let Some(id) = self.reuse_request_id {
                id
            } else {
                let id = RequestId::new(self.next);
                self.next += 1;
                id
            };
            if request.op == RequestOp::Flush {
                self.requests.insert(
                    id,
                    MockRequest {
                        lba: request.lba,
                        pending_polls_remaining: self.pending_polls_before_complete,
                    },
                );
                return Ok(id);
            } else if request.op == RequestOp::Write {
                for segment in request.segments.iter() {
                    let start = request.lba as usize * self.info.device.logical_block_size;
                    let end = start + segment.len;
                    self.storage[start..end].copy_from_slice(segment);
                }
            } else if request.op == RequestOp::Read {
                let mut offset = request.lba as usize * self.info.device.logical_block_size;
                let total = request.block_count as usize * self.info.device.logical_block_size;
                for segment in request.segments.iter_mut() {
                    let len = segment.len.min(total.saturating_sub(
                        offset - request.lba as usize * self.info.device.logical_block_size,
                    ));
                    segment[..len].copy_from_slice(&self.storage[offset..offset + len]);
                    offset += len;
                }
            }
            self.requests.insert(
                id,
                MockRequest {
                    lba: request.lba,
                    pending_polls_remaining: self.pending_polls_before_complete,
                },
            );
            Ok(id)
        }

        fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
            {
                let req = self
                    .requests
                    .get_mut(&request)
                    .ok_or(BlkError::InvalidRequest)?;
                if let Some(hook) = self.repoll_hook.take() {
                    hook.polled.send(request).unwrap();
                    hook.resume.recv().unwrap();
                }
                if req.pending_polls_remaining > 0 {
                    req.pending_polls_remaining -= 1;
                    if self.fail_first_poll_lba == Some(req.lba) {
                        return Err(BlkError::Io);
                    }
                    return Ok(RequestStatus::Pending);
                }
            }
            self.requests.remove(&request);
            Ok(RequestStatus::Complete)
        }
    }

    impl DriverGeneric for MockQueue {
        fn name(&self) -> &str {
            "mock-queue"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
        }
    }

    struct MockInterface {
        name: &'static str,
        queue: Option<Box<dyn IQueue>>,
        owned_queue: Option<QueueHandle>,
        info: QueueInfo,
    }

    impl MockInterface {
        fn new(queue: MockQueue) -> Self {
            let info = queue.info();
            Self {
                name: "mock-rdif",
                queue: Some(Box::new(queue)),
                owned_queue: None,
                info,
            }
        }

        fn new_with_owned(legacy: MockQueue, owned: MockOwnedQueue) -> Self {
            let info = legacy.info();
            Self {
                name: "mock-rdif",
                queue: Some(Box::new(legacy)),
                owned_queue: Some(QueueHandle::new(Box::new(owned))),
                info,
            }
        }
    }

    impl DriverGeneric for MockInterface {
        fn name(&self) -> &str {
            self.name
        }
    }

    impl Interface for MockInterface {
        fn device_info(&self) -> DeviceInfo {
            self.info.device
        }

        fn queue_limits(&self) -> QueueLimits {
            self.info.limits
        }

        fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
            self.queue.take()
        }

        fn create_owned_queue(&mut self) -> Option<QueueHandle> {
            self.owned_queue.take()
        }
    }

    struct MockOwnedQueue {
        info: QueueInfo,
        next: usize,
        storage: Vec<u8>,
        requests: BTreeMap<RequestId, MockOwnedRequest>,
        submitted: Arc<AtomicUsize>,
    }

    struct MockOwnedRequest {
        data: Option<dma_api::InFlightDma>,
        pending_polls_remaining: usize,
    }

    impl MockOwnedQueue {
        fn new(submitted: Arc<AtomicUsize>) -> Self {
            Self {
                info: QueueInfo {
                    id: 0,
                    device: DeviceInfo::new(16, 512),
                    limits: QueueLimits {
                        max_inflight: 8,
                        max_blocks_per_request: 2,
                        max_segments: 1,
                        max_segment_size: 1024,
                        ..QueueLimits::simple(512, u64::MAX)
                    },
                },
                next: 1,
                storage: (0..16 * 512).map(|idx| (idx / 512) as u8).collect(),
                requests: BTreeMap::new(),
                submitted,
            }
        }
    }

    impl IQueueOwned for MockOwnedQueue {
        fn id(&self) -> usize {
            self.info.id
        }

        fn info(&self) -> QueueInfo {
            self.info
        }

        fn submit_request(&mut self, request: OwnedRequest) -> Result<RequestId, SubmitError> {
            if let Err(err) = rdif_block::validate_owned_request(self.info, &request) {
                return Err(SubmitError::new(err, request));
            }
            self.submitted.fetch_add(1, Ordering::AcqRel);
            let id = RequestId::new(self.next);
            self.next += 1;
            let data = if let Some(data) = request.data {
                let mut buffer = data.into_cpu_buffer();
                match request.op {
                    RequestOp::Read => {
                        let start = request.lba as usize * self.info.device.logical_block_size;
                        let end = start
                            + request.block_count as usize * self.info.device.logical_block_size;
                        unsafe {
                            buffer.as_mut_slice_cpu()[..end - start]
                                .copy_from_slice(&self.storage[start..end]);
                        }
                    }
                    RequestOp::Write => {
                        let start = request.lba as usize * self.info.device.logical_block_size;
                        let end = start
                            + request.block_count as usize * self.info.device.logical_block_size;
                        self.storage[start..end].copy_from_slice(buffer.as_slice_cpu());
                    }
                    _ => {}
                }
                Some(unsafe { buffer.prepare_for_device().into_in_flight() })
            } else {
                None
            };
            self.requests.insert(
                id,
                MockOwnedRequest {
                    data,
                    pending_polls_remaining: 1,
                },
            );
            Ok(id)
        }

        fn poll_request(&mut self, request: RequestId) -> Result<RequestPoll, PollError> {
            let req = self
                .requests
                .get_mut(&request)
                .ok_or(PollError::UnknownRequest)?;
            if req.pending_polls_remaining > 0 {
                req.pending_polls_remaining -= 1;
                return Ok(RequestPoll::Pending);
            }
            let req = self.requests.remove(&request).unwrap();
            let data = req
                .data
                .map(|data| unsafe { data.complete_after_quiesce() });
            Ok(RequestPoll::Ready(rdif_block::CompletedRequest::new(
                request,
                Ok(()),
                data,
            )))
        }

        fn cancel_request(&mut self, request: RequestId) -> Result<RequestPoll, PollError> {
            self.poll_request(request)
        }
    }

    #[test]
    fn block_device_read_uses_submit_poll_and_wait_token() {
        let _guard = test_task_guard();
        let bridge = Arc::new(BlockIrqBridge::new());
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_pending_polls_before_complete(2)) as Box<dyn IQueue>],
            bridge.clone(),
            irq_driven_config(),
        )
        .unwrap();

        let fs_dev = device.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = alloc::vec![0u8; 512];
            with_blocking_task(|| fs_dev.read_blocks(3, &mut buf)).unwrap();
            buf
        });
        wait_for_pending_count(&device, 0, 1);
        assert_eq!(drain_queue_hint_until_complete(&device, &bridge, 0, 1), 1);
        let buf = handle.join().unwrap();

        assert_eq!(buf[0], 3);
    }

    #[test]
    fn runtime_builds_sync_device_from_rdif_interface() {
        let _guard = test_task_guard();
        let runtime = BlockRuntime::from_rdif_devices([RdifBlockDevice::new(
            "mock-rdif",
            None,
            Box::new(MockInterface::new(MockQueue::new())),
        )]);
        assert_eq!(runtime.devices().len(), 1);

        let mut buf = alloc::vec![0u8; 512];
        runtime.devices()[0].read_blocks(7, &mut buf).unwrap();

        assert_eq!(buf[0], 7);
    }

    #[test]
    fn runtime_prefers_owned_queue_from_rdif_interface() {
        let _guard = test_task_guard();
        install_test_task_ops();
        let legacy_submits = Arc::new(AtomicUsize::new(0));
        let owned_submits = Arc::new(AtomicUsize::new(0));
        let runtime = BlockRuntime::from_rdif_devices([RdifBlockDevice::new(
            "mock-rdif",
            None,
            Box::new(MockInterface::new_with_owned(
                MockQueue::with_submit_counter(0, legacy_submits.clone()),
                MockOwnedQueue::new(owned_submits.clone()),
            )),
        )]);
        assert_eq!(runtime.devices().len(), 1);

        let mut buf = alloc::vec![0u8; 512];
        runtime.devices()[0].read_blocks(7, &mut buf).unwrap();

        assert_eq!(buf[0], 7);
        assert_eq!(legacy_submits.load(Ordering::Acquire), 0);
        assert_eq!(owned_submits.load(Ordering::Acquire), 1);
    }

    #[test]
    fn block_device_queue_hint_drains_pending_request() {
        let _guard = test_task_guard();
        let bridge = Arc::new(BlockIrqBridge::new());
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_pending_polls_before_complete(2)) as Box<dyn IQueue>],
            bridge.clone(),
            irq_driven_config(),
        )
        .unwrap();

        let fs_dev = device.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = alloc::vec![0u8; 512];
            with_blocking_task(|| fs_dev.read_blocks(4, &mut buf)).unwrap();
            buf
        });
        wait_for_pending_count(&device, 0, 1);
        assert_eq!(drain_queue_hint_until_complete(&device, &bridge, 0, 1), 1);
        let buf = handle.join().unwrap();

        assert_eq!(buf[0], 4);
    }

    #[test]
    fn polling_wait_repolls_without_external_drain_hint() {
        let _guard = test_task_guard();
        let (tx, rx) = mpsc::channel();
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_pending_polls_before_complete(2)) as Box<dyn IQueue>],
            Arc::new(BlockIrqBridge::new()),
            channel_config(tx),
        )
        .unwrap();

        let mut buf = alloc::vec![0u8; 512];
        device.read_blocks(4, &mut buf).unwrap();

        assert_eq!(buf[0], 4);
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(100))
                .is_err()
        );
    }

    #[test]
    fn irq_driven_submit_schedules_initial_drain() {
        let _guard = test_task_guard();
        let bridge = Arc::new(BlockIrqBridge::new());
        let (tx, rx) = mpsc::channel();
        let mut config = channel_config(tx);
        config.completion_mode = BlockCompletionMode::IrqDriven;
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_pending_polls_before_complete(3)) as Box<dyn IQueue>],
            bridge.clone(),
            config,
        )
        .unwrap();

        let fs_dev = device.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = alloc::vec![0u8; 512];
            with_blocking_task(|| fs_dev.read_blocks(5, &mut buf)).unwrap();
            buf
        });

        rx.recv_timeout(std::time::Duration::from_secs(1))
            .expect("IRQ-driven submit must schedule a task-side drain after pending publish");
        assert_eq!(device.drain_events(), 0);
        assert_eq!(device.pending_queue_ready_events(), 0);
        assert_eq!(drain_queue_hint_until_complete(&device, &bridge, 0, 1), 1);

        let buf = handle.join().unwrap();
        assert_eq!(buf[0], 5);
    }

    #[test]
    fn irq_driven_wait_repolls_when_completion_irq_is_lost() {
        let _guard = test_task_guard();
        TEST_TIMEOUT_WAITS.store(0, Ordering::Relaxed);
        let bridge = Arc::new(BlockIrqBridge::new());
        let mut config = irq_driven_config();
        config.submit_window = 1;
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_pending_polls_before_complete(4)) as Box<dyn IQueue>],
            bridge,
            config,
        )
        .unwrap();

        let fs_dev = device.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = alloc::vec![0u8; 512];
            with_blocking_task(|| fs_dev.read_blocks(8, &mut buf)).unwrap();
            buf
        });

        let buf = handle.join().unwrap();

        assert_eq!(buf[0], 8);
        assert_eq!(device.pending_count_for_queue(0), 0);
        assert!(TEST_TIMEOUT_WAITS.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn block_device_queue_hint_releases_still_pending_batch_request_for_later_hint() {
        let _guard = test_task_guard();
        let bridge = Arc::new(BlockIrqBridge::new());
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_retry_while_pending()) as Box<dyn IQueue>],
            bridge.clone(),
            irq_driven_config(),
        )
        .unwrap();

        let fs_dev = device.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = alloc::vec![0u8; 512];
            with_blocking_task(|| fs_dev.read_blocks(6, &mut buf)).unwrap();
            buf
        });
        wait_for_pending_count(&device, 0, 1);

        assert_eq!(device.drain_events(), 0);
        bridge.record_hint(CompletionHint::Queue { queue_id: 0 });
        assert_eq!(device.drain_events(), 0);
        assert_eq!(drain_queue_hint_until_complete(&device, &bridge, 0, 1), 1);
        let buf = handle.join().unwrap();

        assert_eq!(buf[0], 6);
    }

    #[test]
    fn block_device_window_submits_multiple_chunks_before_first_wait() {
        let _guard = test_task_guard();
        let bridge = Arc::new(BlockIrqBridge::new());
        let mut config = irq_driven_config();
        config.submit_window = 3;
        config.max_transfer_bytes = 512;
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_pending_polls_before_complete(2)) as Box<dyn IQueue>],
            bridge.clone(),
            config,
        )
        .unwrap();

        let fs_dev = device.clone();
        let handle = std::thread::spawn(move || {
            let mut buf = alloc::vec![0u8; 3 * 512];
            with_blocking_task(|| fs_dev.read_blocks(1, &mut buf)).unwrap();
            buf
        });
        wait_for_pending_count(&device, 0, 3);
        assert_eq!(device.pending_count_for_queue(0), 3);

        assert_eq!(drain_queue_hint_until_complete(&device, &bridge, 0, 3), 3);
        let buf = handle.join().unwrap();

        assert_eq!(buf[0], 1);
        assert_eq!(buf[512], 2);
        assert_eq!(buf[1024], 3);
    }

    #[test]
    fn block_device_rejects_inflight_driver_request_id_reuse() {
        let _guard = test_task_guard();
        let mut config = noop_config();
        config.submit_window = 2;
        config.max_transfer_bytes = 512;
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_reused_request_id(RequestId::new(1))) as Box<dyn IQueue>],
            Arc::new(BlockIrqBridge::new()),
            config,
        )
        .unwrap();

        let mut buf = alloc::vec![0u8; 2 * 512];
        let result = device.read_blocks(1, &mut buf);

        assert_eq!(result, Err(AxError::InvalidInput));
        assert_eq!(device.pending_count_for_queue(0), 0);
    }

    #[test]
    fn block_device_large_io_distributes_window_across_queues() {
        let _guard = test_task_guard();
        let first_submits = Arc::new(AtomicUsize::new(0));
        let second_submits = Arc::new(AtomicUsize::new(0));
        let mut config = noop_config();
        config.submit_window = 2;
        config.max_transfer_bytes = 512;
        let device = BlockDeviceHandle::new(
            "mock",
            [
                Box::new(MockQueue::with_submit_counter(0, first_submits.clone()))
                    as Box<dyn IQueue>,
                Box::new(MockQueue::with_submit_counter(1, second_submits.clone()))
                    as Box<dyn IQueue>,
            ],
            Arc::new(BlockIrqBridge::new()),
            config,
        )
        .unwrap();

        let mut buf = alloc::vec![0u8; 4 * 512];
        device.read_blocks(1, &mut buf).unwrap();

        assert!(first_submits.load(Ordering::Acquire) > 0);
        assert!(second_submits.load(Ordering::Acquire) > 0);
        assert_eq!(buf[0], 1);
        assert_eq!(buf[3 * 512], 4);
    }

    #[test]
    fn block_device_repoll_during_locked_submit_poll_does_not_deadlock() {
        let _guard = test_task_guard();
        let (polled_tx, polled_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_repoll_hook(polled_tx, resume_rx)) as Box<dyn IQueue>],
            Arc::new(BlockIrqBridge::new()),
            noop_config(),
        )
        .unwrap();

        let fs_dev = device.clone();
        std::thread::spawn(move || {
            let mut buf = alloc::vec![0u8; 512];
            let result = fs_dev.read_blocks(7, &mut buf).map(|()| buf[0]);
            let _ = done_tx.send(result);
        });

        let request_id = polled_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            device.drain_hint(CompletionHint::Request {
                queue_id: 0,
                request_id,
            }),
            0
        );
        resume_tx.send(()).unwrap();

        let result = done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("submit-side repoll must not re-enter the queue lock");
        assert_eq!(result.unwrap(), 7);
    }

    #[test]
    fn block_device_first_poll_error_releases_pending_request() {
        let _guard = test_task_guard();
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_first_poll_failure_on_lba(5)) as Box<dyn IQueue>],
            Arc::new(BlockIrqBridge::new()),
            noop_config(),
        )
        .unwrap();

        let mut buf = alloc::vec![0u8; 512];
        assert!(device.read_blocks(5, &mut buf).is_err());
        assert_eq!(device.pending_count_for_queue(0), 0);
    }

    #[test]
    fn block_device_retry_with_empty_window_returns_without_pending_leak() {
        let _guard = test_task_guard();
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_retry_submits(1)) as Box<dyn IQueue>],
            Arc::new(BlockIrqBridge::new()),
            noop_config(),
        )
        .unwrap();

        let mut buf = alloc::vec![0u8; 512];
        assert!(device.read_blocks(1, &mut buf).is_err());
        assert_eq!(device.pending_count_for_queue(0), 0);
    }

    #[test]
    fn block_device_rejects_duplicate_driver_queue_ids() {
        let _guard = test_task_guard();
        let device = BlockDeviceHandle::new(
            "mock",
            [
                Box::new(MockQueue::with_id(3)) as Box<dyn IQueue>,
                Box::new(MockQueue::with_id(3)) as Box<dyn IQueue>,
            ],
            Arc::new(BlockIrqBridge::new()),
            noop_config(),
        );

        assert!(matches!(device, Err(BlkError::InvalidRequest)));
    }

    #[test]
    fn block_device_rejects_driver_queue_ids_not_representable_by_irq_bits() {
        let _guard = test_task_guard();
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_id(64)) as Box<dyn IQueue>],
            Arc::new(BlockIrqBridge::new()),
            noop_config(),
        );

        assert!(matches!(device, Err(BlkError::InvalidRequest)));
    }

    #[test]
    fn sparse_driver_queue_ids_are_translated_to_dense_runtime_queue_ids() {
        let _guard = test_task_guard();
        let bridge = Arc::new(BlockIrqBridge::new());
        let device = BlockDeviceHandle::new(
            "mock",
            [
                Box::new(MockQueue::with_id(7)) as Box<dyn IQueue>,
                Box::new(MockQueue::with_id(11)) as Box<dyn IQueue>,
            ],
            bridge,
            noop_config(),
        )
        .unwrap();

        device.record_driver_event(rdif_block::Event::from_queue_bits(1 << 11));
        let events = device.bridge().take_events();
        assert_eq!(events.queue_bits, 1 << 1);
    }

    struct StaticIrqHandler {
        event: rdif_block::Event,
    }

    impl rdif_block::IrqHandler for StaticIrqHandler {
        fn handle_irq(&mut self) -> rdif_block::Event {
            self.event
        }
    }

    #[test]
    fn block_irq_action_records_event_without_pending_lock_filter() {
        let _guard = test_task_guard();
        let bridge = Arc::new(BlockIrqBridge::new());
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_id(11)) as Box<dyn IQueue>],
            bridge.clone(),
            irq_driven_config(),
        )
        .unwrap();

        let mut action = BlockIrqAction::new(
            Box::new(StaticIrqHandler {
                event: rdif_block::Event::from_queue_bits(1 << 11),
            }),
            device,
            0,
        );

        assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);

        let events = bridge.take_events();
        assert_eq!(events.queue_bits, 1);
    }

    #[test]
    fn irq_event_before_pending_insert_is_not_dropped() {
        let _guard = test_task_guard();
        let bridge = Arc::new(BlockIrqBridge::new());
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(MockQueue::with_id(0)) as Box<dyn IQueue>],
            bridge,
            irq_driven_config(),
        )
        .unwrap();

        let mut action = BlockIrqAction::new(Box::new(QueueEventIrqHandler), device.clone(), 0);
        assert_eq!(action.run(), crate::os::BlockIrqOutcome::Wake);
        let events = device.bridge().take_events();
        assert_eq!(events.queue_bits, 1);
    }

    struct QueueEventIrqHandler;

    impl rdif_block::IrqHandler for QueueEventIrqHandler {
        fn handle_irq(&mut self) -> rdif_block::Event {
            rdif_block::Event::from_queue_bits(1)
        }
    }

    #[test]
    fn irq_bridge_hint_overflow_falls_back_to_queue_ready_bit() {
        let bridge = BlockIrqBridge::new();
        for id in 0..rdif_block::MAX_COMPLETION_HINTS {
            bridge.record_hint(CompletionHint::Request {
                queue_id: 2,
                request_id: RequestId::new(id),
            });
        }
        bridge.record_hint(CompletionHint::Request {
            queue_id: 2,
            request_id: RequestId::new(99),
        });

        let events = bridge.take_events();
        assert_eq!(events.hints.len(), rdif_block::MAX_COMPLETION_HINTS);
        assert_eq!(events.queue_bits, 1 << 2);
    }

    #[test]
    fn block_device_plans_chunks_with_selected_queue_limits() {
        let _guard = test_task_guard();
        let log = Arc::new(SpinNoIrq::new(Vec::new()));
        let mut first_limits = QueueLimits {
            max_blocks_per_request: 4,
            max_segment_size: 4 * 512,
            ..QueueLimits::simple(512, u64::MAX)
        };
        first_limits.max_inflight = 2;
        first_limits.max_segments = 1;
        let mut second_limits = QueueLimits {
            max_blocks_per_request: 1,
            max_segment_size: 512,
            ..QueueLimits::simple(512, u64::MAX)
        };
        second_limits.max_inflight = 2;
        second_limits.max_segments = 1;
        let mut config = noop_config();
        config.submit_window = 2;
        config.max_transfer_bytes = 4 * 512;
        let device = BlockDeviceHandle::new(
            "mock",
            [
                Box::new(MockQueue::with_limits(0, first_limits)) as Box<dyn IQueue>,
                Box::new(MockQueue::with_request_log_and_limits(
                    1,
                    log.clone(),
                    second_limits,
                )) as Box<dyn IQueue>,
            ],
            Arc::new(BlockIrqBridge::new()),
            config,
        )
        .unwrap();

        let mut buf = alloc::vec![0u8; 4 * 512];
        device.read_blocks(0, &mut buf).unwrap();

        let log = log.lock();
        assert!(
            log.iter()
                .filter(|request| request.queue_id == 1 && request.op == RequestOp::Read)
                .all(|request| request.block_count == 1),
            "second queue requests must obey its one-block limit: {log:?}"
        );
    }

    #[test]
    #[cfg(feature = "ext4")]
    fn block_device_flush_retries_without_returning_wouldblock() {
        let _guard = test_task_guard();
        let mut queue = MockQueue::with_retry_submits(1);
        queue.info.limits.supports_flush = true;
        let device = BlockDeviceHandle::new(
            "mock",
            [Box::new(queue) as Box<dyn IQueue>],
            Arc::new(BlockIrqBridge::new()),
            noop_config(),
        )
        .unwrap();

        let result = device.flush_blocks();

        assert_ne!(result, Err(AxError::WouldBlock));
        assert!(result.is_err());
    }
}
