//! Acceptance and completion observations independent of command ownership.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Condvar, Mutex, PoisonError},
    task::{Context, Poll, Waker},
};

use crate::{AxVmError, AxVmResult, OperationId, sync::MutexExt};

/// Observes one VM command without owning or cancelling its execution.
///
/// Dropping the observation does not cancel the command. Acceptance means the
/// control owner has validated the command; completion means its documented
/// postcondition holds. Synchronous waiting must only occur in task context.
#[must_use = "observe acceptance or completion, or explicitly discard the observation"]
pub struct VmOperation<T> {
    id: OperationId,
    shared: Arc<OperationShared<T>>,
}

struct OperationShared<T> {
    state: Mutex<OperationState<T>>,
    changed: Condvar,
}

struct OperationState<T> {
    accepted: Option<AxVmResult>,
    result: Option<AxVmResult<T>>,
    acceptance_wakers: Vec<Waker>,
    completion_waker: Option<Waker>,
    finished: bool,
}

pub(crate) struct OperationCompletion<T> {
    id: OperationId,
    shared: Arc<OperationShared<T>>,
}

impl<T> VmOperation<T> {
    pub(crate) fn new(id: OperationId) -> (Self, OperationCompletion<T>) {
        let shared = Arc::new(OperationShared {
            state: Mutex::new(OperationState {
                accepted: None,
                result: None,
                acceptance_wakers: Vec::new(),
                completion_waker: None,
                finished: false,
            }),
            changed: Condvar::new(),
        });
        (
            Self {
                id,
                shared: shared.clone(),
            },
            OperationCompletion { id, shared },
        )
    }

    /// Returns the owner-issued operation identity.
    pub const fn id(&self) -> OperationId {
        self.id
    }

    /// Waits asynchronously until validation succeeds or rejects the command.
    pub fn accepted(&self) -> impl Future<Output = AxVmResult> + '_ {
        std::future::poll_fn(|context| {
            let mut state = self.shared.state.lock_unpoisoned();
            if let Some(result) = &state.accepted {
                return Poll::Ready(result.clone());
            }
            if !state
                .acceptance_wakers
                .iter()
                .any(|waker| waker.will_wake(context.waker()))
            {
                state.acceptance_wakers.push(context.waker().clone());
            }
            Poll::Pending
        })
    }

    /// Blocks the calling task until the operation's final postcondition holds.
    pub fn wait(self) -> AxVmResult<T> {
        let mut state = self.shared.state.lock_unpoisoned();
        loop {
            if let Some(result) = state.result.take() {
                return result;
            }
            state = self
                .shared
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl<T> Future for VmOperation<T> {
    type Output = AxVmResult<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.shared.state.lock_unpoisoned();
        if let Some(result) = state.result.take() {
            return Poll::Ready(result);
        }
        assert!(!state.finished, "completed VM operation polled again");
        if state
            .completion_waker
            .as_ref()
            .is_none_or(|waker| !waker.will_wake(context.waker()))
        {
            state.completion_waker = Some(context.waker().clone());
        }
        Poll::Pending
    }
}

impl<T> OperationCompletion<T> {
    pub(crate) const fn id(&self) -> OperationId {
        self.id
    }

    pub(crate) fn accept(&self) {
        let wakers = {
            let mut state = self.shared.state.lock_unpoisoned();
            assert!(state.accepted.is_none(), "operation accepted twice");
            state.accepted = Some(Ok(()));
            std::mem::take(&mut state.acceptance_wakers)
        };
        self.shared.changed.notify_all();
        for waker in wakers {
            waker.wake();
        }
    }

    pub(crate) fn reject(self, error: AxVmError) {
        self.publish(Err(error), true);
    }

    pub(crate) fn finish(self, result: AxVmResult<T>) {
        self.publish(result, false);
    }

    fn publish(&self, result: AxVmResult<T>, rejected: bool) {
        let (acceptance, completion) = {
            let mut state = self.shared.state.lock_unpoisoned();
            assert!(!state.finished, "operation completed twice");
            if state.accepted.is_none() {
                state.accepted = Some(if rejected {
                    Err(result
                        .as_ref()
                        .err()
                        .expect("rejection has an error")
                        .clone())
                } else {
                    Ok(())
                });
            }
            state.result = Some(result);
            state.finished = true;
            (
                std::mem::take(&mut state.acceptance_wakers),
                state.completion_waker.take(),
            )
        };
        // Wakers may reenter application code; the state mutex is released.
        self.shared.changed.notify_all();
        for waker in acceptance.into_iter().chain(completion) {
            waker.wake();
        }
    }
}

impl<T> Drop for OperationCompletion<T> {
    fn drop(&mut self) {
        if !self.shared.state.lock_unpoisoned().finished {
            self.publish(
                Err(AxVmError::OperationCancelled { operation: self.id }),
                true,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Barrier, mpsc},
        task::Wake,
        thread,
    };

    use super::*;
    use crate::VmKey;

    struct Observer(mpsc::Sender<()>);

    impl Wake for Observer {
        fn wake(self: Arc<Self>) {
            self.0.send(()).unwrap();
        }
    }

    fn operation<T>() -> (VmOperation<T>, OperationCompletion<T>) {
        VmOperation::new(OperationId::new(VmKey::new(1, 1), 1))
    }

    #[test]
    fn acceptance_and_completion_have_distinct_progress() {
        let (mut operation, completion) = operation::<usize>();
        let (sender, receiver) = mpsc::channel();
        let waker = Waker::from(Arc::new(Observer(sender)));
        let mut context = Context::from_waker(&waker);
        {
            let mut accepted = std::pin::pin!(operation.accepted());
            assert!(accepted.as_mut().poll(&mut context).is_pending());
            completion.accept();
            receiver.recv().unwrap();
            assert_eq!(accepted.as_mut().poll(&mut context), Poll::Ready(Ok(())));
        }
        assert!(Pin::new(&mut operation).poll(&mut context).is_pending());
        completion.finish(Ok(17));
        receiver.recv().unwrap();
        assert_eq!(
            Pin::new(&mut operation).poll(&mut context),
            Poll::Ready(Ok(17))
        );
    }

    #[test]
    fn owner_completion_progresses_after_observer_disconnection() {
        let (operation, completion) = operation::<usize>();
        let shared = completion.shared.clone();
        drop(operation);
        completion.accept();
        completion.finish(Ok(23));
        assert_eq!(shared.state.lock_unpoisoned().result.take(), Some(Ok(23)));
    }

    #[test]
    fn blocking_observer_and_owner_failure_always_complete() {
        let (operation, completion) = operation::<()>();
        let ready = Arc::new(Barrier::new(2));
        let observer_ready = ready.clone();
        let observer = thread::spawn(move || {
            observer_ready.wait();
            operation.wait()
        });
        ready.wait();
        let id = completion.id();
        drop(completion);
        assert_eq!(
            observer.join().unwrap(),
            Err(AxVmError::OperationCancelled { operation: id })
        );
    }

    #[test]
    fn rejected_command_never_reports_successful_acceptance() {
        let (operation, completion) = operation::<()>();
        let error = AxVmError::EntryClosed {
            vm: operation.id().vm(),
        };
        completion.reject(error.clone());
        {
            let mut accepted = std::pin::pin!(operation.accepted());
            let mut context = Context::from_waker(Waker::noop());
            assert_eq!(
                accepted.as_mut().poll(&mut context),
                Poll::Ready(Err(error.clone()))
            );
        }
        assert_eq!(operation.wait(), Err(error));
    }
}
