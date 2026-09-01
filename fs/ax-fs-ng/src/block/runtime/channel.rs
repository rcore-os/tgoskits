use alloc::{collections::VecDeque, sync::Arc};

use super::waiters::CapacityWaiters;
use crate::{
    BlockError,
    os::{BlockNotification, runtime_ops, sync::IrqMutex},
};

pub(super) enum SendError<T> {
    Full(T),
    Closed(T),
}

pub(super) struct BoundedChannel<T> {
    state: IrqMutex<ChannelState<T>>,
    item_ready: Arc<dyn BlockNotification>,
    space_waiters: CapacityWaiters,
}

struct ChannelState<T> {
    queue: VecDeque<T>,
    capacity: usize,
    closed: bool,
}

impl<T> BoundedChannel<T> {
    pub(super) fn with_item_notification(
        capacity: usize,
        item_ready: Arc<dyn BlockNotification>,
    ) -> Result<Self, BlockError> {
        if capacity == 0 {
            return Err(BlockError::InvalidRequest);
        }
        Ok(Self::new(capacity, item_ready))
    }

    fn new(capacity: usize, item_ready: Arc<dyn BlockNotification>) -> Self {
        Self {
            state: IrqMutex::new(ChannelState {
                queue: VecDeque::with_capacity(capacity),
                capacity,
                closed: false,
            }),
            item_ready,
            space_waiters: CapacityWaiters::new(),
        }
    }

    pub(super) fn send(&self, value: T, nowait: bool) -> Result<(), SendError<T>> {
        loop {
            let mut state = self.state.lock();
            if state.closed {
                return Err(SendError::Closed(value));
            }
            if state.queue.len() < state.capacity {
                state.queue.push_back(value);
                let available = state.capacity - state.queue.len();
                drop(state);
                self.item_ready.notify();
                self.space_waiters.notify_available(available);
                return Ok(());
            }
            drop(state);

            let can_block = runtime_ops().is_ok_and(|ops| ops.can_block());
            if nowait || !can_block {
                return Err(SendError::Full(value));
            }
            if self
                .space_waiters
                .wait_for(1, || {
                    let state = self.state.lock();
                    if state.closed {
                        state.capacity
                    } else {
                        state.capacity - state.queue.len()
                    }
                })
                .is_err()
            {
                return Err(SendError::Full(value));
            }
        }
    }

    pub(super) fn send_many(
        &self,
        mut values: VecDeque<T>,
        nowait: bool,
    ) -> Result<(), SendError<VecDeque<T>>> {
        if values.is_empty() {
            return Ok(());
        }
        if values.len() > self.state.lock().capacity {
            return Err(SendError::Full(values));
        }
        loop {
            let available = {
                let mut state = self.state.lock();
                if state.closed {
                    return Err(SendError::Closed(values));
                }
                if state.capacity - state.queue.len() >= values.len() {
                    state.queue.append(&mut values);
                    Some(state.capacity - state.queue.len())
                } else {
                    None
                }
            };
            if let Some(available) = available {
                self.item_ready.notify();
                self.space_waiters.notify_available(available);
                return Ok(());
            }

            let can_block = runtime_ops().is_ok_and(|ops| ops.can_block());
            if nowait || !can_block {
                return Err(SendError::Full(values));
            }
            if self
                .space_waiters
                .wait_for(values.len(), || {
                    let state = self.state.lock();
                    if state.closed {
                        state.capacity
                    } else {
                        state.capacity - state.queue.len()
                    }
                })
                .is_err()
            {
                return Err(SendError::Full(values));
            }
        }
    }

    pub(super) fn try_recv(&self) -> Option<T> {
        let (value, available) = {
            let mut state = self.state.lock();
            let value = state.queue.pop_front();
            let available = state.capacity - state.queue.len();
            (value, available)
        };
        if value.is_some() {
            self.space_waiters.notify_available(available);
        }
        value
    }

    pub(super) fn try_recv_many(&self, values: &mut VecDeque<T>, limit: usize) -> usize {
        if limit == 0 {
            return 0;
        }
        let (received, available) = {
            let mut state = self.state.lock();
            let received = limit.min(state.queue.len());
            values.extend(state.queue.drain(..received));
            (received, state.capacity - state.queue.len())
        };
        if received != 0 {
            self.space_waiters.notify_available(available);
        }
        received
    }

    #[cfg(test)]
    pub(super) fn recv(&self) -> Option<T> {
        loop {
            let received = {
                let mut state = self.state.lock();
                if let Some(value) = state.queue.pop_front() {
                    Some((value, state.capacity - state.queue.len()))
                } else {
                    if state.closed {
                        return None;
                    }
                    None
                }
            };
            if let Some((value, available)) = received {
                self.space_waiters.notify_available(available);
                return Some(value);
            }
            self.item_ready.wait();
        }
    }

    pub(super) fn close(&self) {
        self.state.lock().closed = true;
        self.item_ready.notify();
        self.space_waiters.notify_all();
    }

    pub(super) fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    pub(super) fn is_closed_and_empty(&self) -> bool {
        let state = self.state.lock();
        state.closed && state.queue.is_empty()
    }

    #[cfg(test)]
    fn blocked_sender_count(&self) -> usize {
        self.space_waiters.len()
    }

    #[cfg(test)]
    pub(super) fn queued_len(&self) -> usize {
        self.state.lock().queue.len()
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::time::Duration;
    use std::{
        sync::{Condvar, Mutex, mpsc},
        thread,
    };

    use super::*;

    struct WindowNotification {
        pending: Mutex<bool>,
        ready: Condvar,
        entered_wait: Mutex<usize>,
        entered_ready: Condvar,
    }

    impl WindowNotification {
        fn new() -> Self {
            Self {
                pending: Mutex::new(false),
                ready: Condvar::new(),
                entered_wait: Mutex::new(0),
                entered_ready: Condvar::new(),
            }
        }

        fn wait_until_waiter_count(&self, count: usize) {
            let mut entered = self.entered_wait.lock().unwrap();
            while *entered < count {
                entered = self.entered_ready.wait(entered).unwrap();
            }
        }

        fn publish(&self) {
            *self.pending.lock().unwrap() = true;
            self.ready.notify_one();
        }
    }

    impl BlockNotification for WindowNotification {
        fn notify(&self) {
            self.publish();
        }

        fn notify_from_irq(&self) {
            self.publish();
        }

        #[track_caller]
        fn wait(&self) {
            *self.entered_wait.lock().unwrap() += 1;
            self.entered_ready.notify_one();
            let mut pending = self.pending.lock().unwrap();
            while !*pending {
                pending = self.ready.wait(pending).unwrap();
            }
            *pending = false;
        }

        #[track_caller]
        fn wait_timeout(&self, duration: Duration) -> bool {
            let mut pending = self.pending.lock().unwrap();
            if !*pending {
                let (next, timeout) = self.ready.wait_timeout(pending, duration).unwrap();
                pending = next;
                if timeout.timed_out() && !*pending {
                    return true;
                }
            }
            *pending = false;
            false
        }
    }

    #[test]
    fn notification_between_empty_check_and_sleep_is_not_lost() {
        crate::os::task::install_test_runtime_ops();
        let notification = Arc::new(WindowNotification::new());
        let channel =
            Arc::new(BoundedChannel::with_item_notification(1, notification.clone()).unwrap());
        let receiver = Arc::clone(&channel);
        let join = thread::spawn(move || receiver.recv());

        notification.wait_until_waiter_count(1);
        assert!(channel.send(17, false).is_ok());
        assert_eq!(join.join().unwrap(), Some(17));
    }

    #[test]
    fn full_channel_rejects_nowait_and_blocks_regular_sender() {
        crate::os::task::install_test_runtime_ops();
        let notification = Arc::new(WindowNotification::new());
        let channel = Arc::new(BoundedChannel::with_item_notification(1, notification).unwrap());
        assert!(channel.send(1, false).is_ok());

        match channel.send(2, true) {
            Err(SendError::Full(2)) => {}
            _ => panic!("NOWAIT submission did not report a full channel"),
        }

        let sender = Arc::clone(&channel);
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let join = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = sender.send(3, false);
            done_tx.send(result.is_ok()).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
        assert_eq!(channel.recv(), Some(1));
        assert!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        assert_eq!(channel.recv(), Some(3));
        join.join().unwrap();
    }

    #[test]
    fn batch_receive_releases_channel_capacity() {
        let item_ready: Arc<dyn BlockNotification> = Arc::new(WindowNotification::new());
        let channel = BoundedChannel::new(4, item_ready);
        assert!(
            channel
                .send_many(VecDeque::from([1, 2, 3, 4]), true)
                .is_ok()
        );

        let mut received = VecDeque::with_capacity(4);
        assert_eq!(channel.try_recv_many(&mut received, 4), 4);

        assert_eq!(received, VecDeque::from([1, 2, 3, 4]));
        assert_eq!(channel.blocked_sender_count(), 0);
    }

    #[test]
    fn batch_receive_wakes_every_blocked_sender() {
        crate::os::task::install_test_runtime_ops();
        let item_ready: Arc<dyn BlockNotification> = Arc::new(WindowNotification::new());
        let channel = Arc::new(BoundedChannel::new(4, item_ready));
        assert!(
            channel
                .send_many(VecDeque::from([0, 1, 2, 3]), true)
                .is_ok()
        );

        let (done_tx, done_rx) = mpsc::channel();
        let mut joins = Vec::new();
        for value in 4..8 {
            let sender = Arc::clone(&channel);
            let done_tx = done_tx.clone();
            joins.push(thread::spawn(move || {
                assert!(sender.send(value, false).is_ok());
                done_tx.send(()).unwrap();
            }));
        }
        drop(done_tx);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while channel.blocked_sender_count() != 4 {
            assert!(
                std::time::Instant::now() < deadline,
                "senders did not enter the capacity wait set"
            );
            thread::yield_now();
        }

        let mut received = VecDeque::new();
        assert_eq!(channel.try_recv_many(&mut received, 4), 4);
        for _ in 0..4 {
            done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        for join in joins {
            join.join().unwrap();
        }
    }

    #[test]
    fn one_released_slot_wakes_only_one_blocked_sender() {
        crate::os::task::install_test_runtime_ops();
        let item_ready: Arc<dyn BlockNotification> = Arc::new(WindowNotification::new());
        let channel = Arc::new(BoundedChannel::new(4, item_ready));
        assert!(
            channel
                .send_many(VecDeque::from([0, 1, 2, 3]), true)
                .is_ok()
        );

        let (done_tx, done_rx) = mpsc::channel();
        let mut joins = Vec::new();
        for value in 4..8 {
            let sender = Arc::clone(&channel);
            let done_tx = done_tx.clone();
            joins.push(thread::spawn(move || {
                assert!(sender.send(value, false).is_ok());
                done_tx.send(()).unwrap();
            }));
        }
        drop(done_tx);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while channel.blocked_sender_count() != 4 {
            assert!(
                std::time::Instant::now() < deadline,
                "senders did not enter the capacity wait set"
            );
            thread::yield_now();
        }

        assert!(channel.try_recv().is_some());
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(channel.blocked_sender_count(), 3);
        assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());

        let mut received = VecDeque::new();
        assert_eq!(channel.try_recv_many(&mut received, 4), 4);
        for _ in 0..3 {
            done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        for join in joins {
            join.join().unwrap();
        }
    }

    #[test]
    fn released_capacity_skips_a_batch_that_cannot_fit() {
        crate::os::task::install_test_runtime_ops();
        let item_ready: Arc<dyn BlockNotification> = Arc::new(WindowNotification::new());
        let channel = Arc::new(BoundedChannel::new(4, item_ready));
        assert!(
            channel
                .send_many(VecDeque::from([0, 1, 2, 3]), true)
                .is_ok()
        );

        let batch_sender = Arc::clone(&channel);
        let (batch_tx, batch_rx) = mpsc::channel();
        let batch_join = thread::spawn(move || {
            assert!(
                batch_sender
                    .send_many(VecDeque::from([4, 5, 6, 7]), false)
                    .is_ok()
            );
            batch_tx.send(()).unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while channel.blocked_sender_count() != 1 {
            assert!(
                std::time::Instant::now() < deadline,
                "batch sender did not enter the capacity wait set"
            );
            thread::yield_now();
        }

        let single_sender = Arc::clone(&channel);
        let (single_tx, single_rx) = mpsc::channel();
        let single_join = thread::spawn(move || {
            assert!(single_sender.send(8, false).is_ok());
            single_tx.send(()).unwrap();
        });
        while channel.blocked_sender_count() != 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "single sender did not enter the capacity wait set"
            );
            thread::yield_now();
        }

        assert!(channel.try_recv().is_some());
        single_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(batch_rx.recv_timeout(Duration::from_millis(20)).is_err());

        let mut received = VecDeque::new();
        assert_eq!(channel.try_recv_many(&mut received, 4), 4);
        batch_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        single_join.join().unwrap();
        batch_join.join().unwrap();
    }
}
