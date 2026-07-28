use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

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

    fn pair_with_notification(
        notification: Arc<dyn BlockNotification>,
    ) -> (Self, CompletionSender) {
        let cell = Arc::new(CompletionCell {
            state: IrqMutex::new(CompletionState {
                result: None,
                receiver_alive: true,
            }),
            notification,
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
            self.cell.notification.wait();
        }
    }
}

impl CompletionGroup {
    pub(super) fn pairs(count: usize) -> Result<(Self, VecDeque<CompletionSender>), BlkError> {
        if count == 0 {
            return Err(BlkError::InvalidRequest);
        }
        let mut subscriptions = Vec::new();
        subscriptions
            .try_reserve_exact(count)
            .map_err(|_| BlkError::NoMemory)?;
        let mut senders = VecDeque::new();
        senders
            .try_reserve_exact(count)
            .map_err(|_| BlkError::NoMemory)?;
        let notification = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .notification();
        for _ in 0..count {
            let (subscription, sender) =
                CompletionSubscription::pair_with_notification(Arc::clone(&notification));
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
            drop(state);
            self.cell.notification.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::CompletionGroup;

    #[test]
    fn completion_group_coalesces_wakeups_on_one_notification() {
        crate::os::task::install_test_runtime_ops();
        let (group, _senders) = CompletionGroup::pairs(4).unwrap();
        let first = &group.subscriptions[0].cell.notification;

        assert!(
            group
                .subscriptions
                .iter()
                .all(|subscription| Arc::ptr_eq(first, &subscription.cell.notification))
        );
    }
}
