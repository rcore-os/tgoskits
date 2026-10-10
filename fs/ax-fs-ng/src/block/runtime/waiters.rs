#[cfg(test)]
use alloc::boxed::Box;
use alloc::{sync::Arc, vec::Vec};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
};

use atomic_waker::AtomicWaker;

pub(super) use crate::os::waiters::TaskWaiters;
use crate::{
    BlockResult,
    os::{BlockNotification, runtime_ops, sync::RawSpinLock},
};

struct AsyncWaiterState {
    notified: core::sync::atomic::AtomicBool,
    waker: AtomicWaker,
}

struct AsyncWaiterInner {
    waiters: RawSpinLock<Vec<Arc<AsyncWaiterState>>>,
}

/// A lock-safe asynchronous waiter registry.
pub(super) struct AsyncWaiters {
    inner: Arc<AsyncWaiterInner>,
}

/// One one-shot asynchronous wait registration.
pub(super) struct AsyncWaiter {
    inner: Arc<AsyncWaiterInner>,
    state: Arc<AsyncWaiterState>,
}

impl AsyncWaiters {
    pub(super) fn new() -> Self {
        Self {
            inner: Arc::new(AsyncWaiterInner {
                waiters: RawSpinLock::new(Vec::new()),
            }),
        }
    }

    pub(super) fn listen(&self) -> AsyncWaiter {
        let state = Arc::new(AsyncWaiterState {
            notified: core::sync::atomic::AtomicBool::new(false),
            waker: AtomicWaker::new(),
        });
        self.inner.waiters.lock_irqsave().push(Arc::clone(&state));
        AsyncWaiter {
            inner: Arc::clone(&self.inner),
            state,
        }
    }

    pub(super) fn notify_all(&self) {
        let waiters = core::mem::take(&mut *self.inner.waiters.lock_irqsave());
        for waiter in waiters {
            waiter.notified.store(true, Ordering::Release);
            waiter.waker.wake();
        }
    }
}

impl AsyncWaiter {
    fn remove(&self) {
        let mut waiters = self.inner.waiters.lock_irqsave();
        if let Some(index) = waiters
            .iter()
            .position(|waiter| Arc::ptr_eq(waiter, &self.state))
        {
            waiters.swap_remove(index);
        }
    }
}

impl Future for AsyncWaiter {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.state.waker.register(context.waker());
        if self.state.notified.load(Ordering::Acquire) {
            self.remove();
            drop(self.state.waker.take());
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for AsyncWaiter {
    fn drop(&mut self) {
        self.remove();
        drop(self.state.waker.take());
    }
}

struct CapacityWaiter {
    required: usize,
    notification: Arc<dyn BlockNotification>,
}

/// Task waiters blocked on bounded-channel capacity.
///
/// Unlike a broadcast wait set, this registry wakes only a set of producers
/// whose requests can fit in the newly available capacity. A producer that
/// still cannot fit hands unused capacity to smaller waiters before sleeping.
pub(super) struct CapacityWaiters {
    waiters: RawSpinLock<Vec<CapacityWaiter>>,
    count: AtomicUsize,
    #[cfg(test)]
    registration_hook: RawSpinLock<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    detach_hook: RawSpinLock<Option<Box<dyn FnOnce() + Send>>>,
}

impl CapacityWaiters {
    pub(super) const fn new() -> Self {
        Self {
            waiters: RawSpinLock::new(Vec::new()),
            count: AtomicUsize::new(0),
            #[cfg(test)]
            registration_hook: RawSpinLock::new(None),
            #[cfg(test)]
            detach_hook: RawSpinLock::new(None),
        }
    }

    pub(super) fn wait_for(
        &self,
        required: usize,
        available: impl FnOnce() -> usize,
    ) -> BlockResult {
        let notification = runtime_ops()?.notification();
        {
            let mut waiters = self.waiters.lock_irqsave();
            waiters.push(CapacityWaiter {
                required,
                notification: Arc::clone(&notification),
            });
            self.count.store(waiters.len(), Ordering::Release);
        }
        #[cfg(test)]
        self.run_registration_hook();

        let available = available();
        if available >= required {
            self.remove(&notification);
        } else {
            // This waiter cannot use a partial gap, but a smaller waiter may.
            self.notify_available(available);
            notification.wait();
            self.remove(&notification);
        }
        Ok(())
    }

    pub(super) fn notify_available(&self, mut available: usize) {
        if available == 0 {
            return;
        }

        let notifications = {
            let mut waiters = self.waiters.lock_irqsave();
            let mut notifications = Vec::new();
            let mut index = 0;
            while index < waiters.len() && available != 0 {
                if waiters[index].required <= available {
                    let waiter = waiters.remove(index);
                    available -= waiter.required;
                    notifications.push(waiter.notification);
                } else {
                    index += 1;
                }
            }
            self.count.store(waiters.len(), Ordering::Release);
            notifications
        };
        for notification in notifications {
            notification.notify();
        }
    }

    pub(super) fn notify_all(&self) {
        let waiters = {
            let mut waiters = self.waiters.lock_irqsave();
            let detached = core::mem::take(&mut *waiters);
            self.count.store(0, Ordering::Release);
            detached
        };
        #[cfg(test)]
        self.run_detach_hook();
        for waiter in waiters {
            waiter.notification.notify();
        }
    }

    fn remove(&self, notification: &Arc<dyn BlockNotification>) {
        let mut waiters = self.waiters.lock_irqsave();
        if let Some(index) = waiters
            .iter()
            .position(|waiter| Arc::ptr_eq(&waiter.notification, notification))
        {
            waiters.remove(index);
            self.count.store(waiters.len(), Ordering::Release);
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn set_registration_hook(&self, hook: impl FnOnce() + Send + 'static) {
        let previous = self
            .registration_hook
            .lock_irqsave()
            .replace(alloc::boxed::Box::new(hook));
        assert!(
            previous.is_none(),
            "capacity registration hook already installed"
        );
    }

    #[cfg(test)]
    fn run_registration_hook(&self) {
        let hook = self.registration_hook.lock_irqsave().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_detach_hook(&self, hook: impl FnOnce() + Send + 'static) {
        let previous = self.detach_hook.lock_irqsave().replace(Box::new(hook));
        assert!(previous.is_none(), "detach hook already installed");
    }

    #[cfg(test)]
    fn run_detach_hook(&self) {
        let hook = self.detach_hook.lock_irqsave().take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::{
        future::Future,
        task::{Context, Poll, Waker},
    };
    use std::{sync::mpsc, task::Wake, time::Duration};

    use super::*;

    struct CountingNotification {
        wakes: AtomicUsize,
    }

    impl BlockNotification for CountingNotification {
        fn notify(&self) {
            self.wakes.fetch_add(1, Ordering::Relaxed);
        }

        fn wait(&self) {
            unreachable!("the notification is never waited on in this test");
        }

        fn wait_timeout(&self, _: Duration) -> bool {
            unreachable!("the notification is never waited on in this test");
        }
    }

    #[test]
    fn available_capacity_notifies_registered_waiter_when_count_is_stale() {
        let registry = CapacityWaiters::new();
        let notification = Arc::new(CountingNotification {
            wakes: AtomicUsize::new(0),
        });
        registry.waiters.lock_irqsave().push(CapacityWaiter {
            required: 1,
            notification: notification.clone(),
        });

        // A count load may still observe zero after registration has stored
        // one, so the notification path must inspect the locked queue.
        registry.count.store(0, Ordering::Relaxed);
        registry.notify_available(1);
        assert_eq!(notification.wakes.load(Ordering::Relaxed), 1);
        assert!(registry.waiters.lock_irqsave().is_empty());
    }

    #[test]
    fn capacity_waiter_registered_after_detach_remains_visible() {
        let registry = Arc::new(CapacityWaiters::new());
        let first = Arc::new(CountingNotification {
            wakes: AtomicUsize::new(0),
        });
        registry.waiters.lock_irqsave().push(CapacityWaiter {
            required: 1,
            notification: first.clone(),
        });
        registry.count.store(1, Ordering::Release);

        let second = Arc::new(CountingNotification {
            wakes: AtomicUsize::new(0),
        });
        let publisher = Arc::clone(&registry);
        let second_notification = Arc::clone(&second);
        registry.set_detach_hook(move || {
            let mut waiters = publisher.waiters.lock_irqsave();
            waiters.push(CapacityWaiter {
                required: 1,
                notification: second_notification,
            });
            publisher.count.store(waiters.len(), Ordering::Release);
        });

        registry.notify_all();
        assert_eq!(first.wakes.load(Ordering::Relaxed), 1);
        registry.notify_available(1);
        assert_eq!(second.wakes.load(Ordering::Relaxed), 1);
    }

    struct ReentrantWake {
        waiters: Arc<AsyncWaiters>,
        done: mpsc::Sender<()>,
    }

    impl Wake for ReentrantWake {
        fn wake(self: Arc<Self>) {
            let waiter = self.waiters.listen();
            drop(waiter);
            self.done.send(()).unwrap();
        }
    }

    #[test]
    fn async_waiter_wake_allows_reentrant_registration() {
        let waiters = Arc::new(AsyncWaiters::new());
        let mut listener = Box::pin(waiters.listen());
        let (done_tx, done_rx) = mpsc::channel();
        let waker = Waker::from(Arc::new(ReentrantWake {
            waiters: Arc::clone(&waiters),
            done: done_tx,
        }));
        let mut context = Context::from_waker(&waker);
        assert!(matches!(
            listener.as_mut().poll(&mut context),
            Poll::Pending
        ));

        let publisher = Arc::clone(&waiters);
        std::thread::spawn(move || publisher.notify_all());
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("waiter wake must release the registry before calling Waker");
    }
}
