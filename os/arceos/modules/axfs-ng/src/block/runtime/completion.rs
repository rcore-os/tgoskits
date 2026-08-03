use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use rdif_block::{BlkError, CompletedRequest};

use crate::os::{BlockNotification, runtime_ops, sync::IrqMutex};

/// Blocking one-shot receiver for one owned block request.
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
    group: Arc<CompletionBarrier>,
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
            group,
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
    /// No polling or nonblocking receive API is provided.
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
        let mut state = self.cell.state.lock();
        state.receiver_alive = false;
        drop(state.result.take());
    }
}

impl CompletionSender {
    pub(super) fn complete(self, request: CompletedRequest) {
        let mut state = self.cell.state.lock();
        if state.receiver_alive {
            state.result = Some(request);
        }
        drop(state);

        // Every group receiver waits for all members, so publishing an
        // intermediate member cannot unblock useful work. The AcqRel
        // countdown makes the final publisher the single notification owner.
        if self.cell.group.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.cell.group.notification.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use rdif_block::{CompletedRequest, RequestId};

    use super::{BlockNotification, CompletionGroup};

    #[derive(Default)]
    struct CountingNotification {
        notifications: AtomicUsize,
    }

    impl BlockNotification for CountingNotification {
        fn notify(&self) {
            self.notifications.fetch_add(1, Ordering::Relaxed);
        }

        fn notify_from_irq(&self) {
            self.notify();
        }

        fn wait(&self) {
            unreachable!("the completion publisher test does not block")
        }

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
}
