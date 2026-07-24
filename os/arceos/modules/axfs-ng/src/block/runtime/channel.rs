use alloc::{collections::VecDeque, sync::Arc};

use crate::os::{BlockNotification, runtime_ops, sync::IrqMutex};

pub(super) enum SendError<T> {
    Full(T),
    Closed(T),
}

pub(super) struct BoundedChannel<T> {
    state: IrqMutex<ChannelState<T>>,
    item_ready: Arc<dyn BlockNotification>,
    space_ready: Arc<dyn BlockNotification>,
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
    ) -> Result<Self, ax_errno::AxError> {
        if capacity == 0 {
            return Err(ax_errno::AxError::InvalidInput);
        }
        let ops = runtime_ops()?;
        Ok(Self {
            state: IrqMutex::new(ChannelState {
                queue: VecDeque::with_capacity(capacity),
                capacity,
                closed: false,
            }),
            item_ready,
            space_ready: ops.notification(),
        })
    }

    pub(super) fn send(&self, value: T, nowait: bool) -> Result<(), SendError<T>> {
        loop {
            {
                let mut state = self.state.lock();
                if state.closed {
                    return Err(SendError::Closed(value));
                }
                if state.queue.len() < state.capacity {
                    state.queue.push_back(value);
                    self.item_ready.notify();
                    return Ok(());
                }
            }

            let can_block = runtime_ops().is_ok_and(|ops| ops.can_block());
            if nowait || !can_block {
                return Err(SendError::Full(value));
            }
            self.space_ready.wait();
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
            {
                let mut state = self.state.lock();
                if state.closed {
                    return Err(SendError::Closed(values));
                }
                if state.capacity - state.queue.len() >= values.len() {
                    state.queue.append(&mut values);
                    self.item_ready.notify();
                    return Ok(());
                }
            }

            let can_block = runtime_ops().is_ok_and(|ops| ops.can_block());
            if nowait || !can_block {
                return Err(SendError::Full(values));
            }
            self.space_ready.wait();
        }
    }

    pub(super) fn try_recv(&self) -> Option<T> {
        let value = self.state.lock().queue.pop_front();
        if value.is_some() {
            self.space_ready.notify();
        }
        value
    }

    #[cfg(test)]
    pub(super) fn recv(&self) -> Option<T> {
        loop {
            {
                let mut state = self.state.lock();
                if let Some(value) = state.queue.pop_front() {
                    self.space_ready.notify();
                    return Some(value);
                }
                if state.closed {
                    return None;
                }
            }
            self.item_ready.wait();
        }
    }

    pub(super) fn close(&self) {
        self.state.lock().closed = true;
        self.item_ready.notify();
        self.space_ready.notify();
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
        entered_wait: Mutex<bool>,
        entered_ready: Condvar,
    }

    impl WindowNotification {
        fn new() -> Self {
            Self {
                pending: Mutex::new(false),
                ready: Condvar::new(),
                entered_wait: Mutex::new(false),
                entered_ready: Condvar::new(),
            }
        }

        fn wait_until_receiver_checked_empty(&self) {
            let mut entered = self.entered_wait.lock().unwrap();
            while !*entered {
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

        fn wait(&self) {
            *self.entered_wait.lock().unwrap() = true;
            self.entered_ready.notify_one();
            let mut pending = self.pending.lock().unwrap();
            while !*pending {
                pending = self.ready.wait(pending).unwrap();
            }
            *pending = false;
        }

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

        notification.wait_until_receiver_checked_empty();
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
}
