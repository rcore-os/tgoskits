//! Single-threaded cooperative future executor core.

use alloc::{
    boxed::Box,
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, Weak},
    task::Wake,
};
use core::{
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use crate::sync::SpinMutex;

type LocalFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

struct LocalTask {
    future: SpinMutex<Option<LocalFuture>>,
    queued: AtomicBool,
    completed: AtomicBool,
    executor: Weak<LocalExecutorInner>,
}

// SAFETY: `LocalTask` is shared with `Waker`, so wake callbacks may run on
// another CPU. Those callbacks only touch atomics and the executor ready queue.
// The non-Send future is only accessed from `LocalExecutorCore::poll_ready`;
// `LocalExecutorCore` and `LocalSpawnerCore` are deliberately !Send.
unsafe impl Send for LocalTask {}
// SAFETY: same as the `Send` impl: shared access through a waker never polls or
// otherwise touches the non-Send future.
unsafe impl Sync for LocalTask {}

impl LocalTask {
    fn poll(self: &Arc<Self>) {
        if self.completed.load(Ordering::Acquire) {
            return;
        }
        let waker = Waker::from(self.clone());
        let mut cx = Context::from_waker(&waker);
        let completed = {
            let mut future_slot = self.future.lock();
            let Some(future) = future_slot.as_mut() else {
                return;
            };
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(()) => {
                    *future_slot = None;
                    true
                }
                Poll::Pending => false,
            }
        };
        if completed {
            self.completed.store(true, Ordering::Release);
            if let Some(executor) = self.executor.upgrade() {
                executor.task_count.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    fn enqueue(self: &Arc<Self>) {
        if self.completed.load(Ordering::Acquire) {
            return;
        }
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(executor) = self.executor.upgrade() {
            executor.ready.lock().push_back(self.clone());
            (executor.pend)();
        }
    }
}

impl Wake for LocalTask {
    fn wake(self: Arc<Self>) {
        self.enqueue();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.enqueue();
    }
}

struct LocalExecutorInner {
    ready: SpinMutex<VecDeque<Arc<LocalTask>>>,
    active: AtomicBool,
    task_count: AtomicUsize,
    pend: Arc<dyn Fn() + Send + Sync + 'static>,
}

/// Single-threaded cooperative future executor core.
#[derive(Clone)]
pub struct LocalExecutorCore {
    inner: Arc<LocalExecutorInner>,
    _not_send: PhantomData<Rc<()>>,
}

impl LocalExecutorCore {
    /// Creates an empty local executor core.
    pub fn new(pend: Arc<dyn Fn() + Send + Sync + 'static>) -> Self {
        Self {
            inner: Arc::new(LocalExecutorInner {
                ready: SpinMutex::new(VecDeque::new()),
                active: AtomicBool::new(false),
                task_count: AtomicUsize::new(0),
                pend,
            }),
            _not_send: PhantomData,
        }
    }

    /// Returns a spawner tied to this executor.
    pub fn spawner(&self) -> LocalSpawnerCore {
        LocalSpawnerCore {
            inner: self.inner.clone(),
            _not_send: PhantomData,
        }
    }

    /// Enters this executor, rejecting reentrant runs.
    pub fn enter(&self) {
        assert!(
            !self.inner.active.swap(true, Ordering::AcqRel),
            "local executor cannot be run reentrantly"
        );
    }

    /// Leaves this executor.
    pub fn leave(&self) {
        self.inner.active.store(false, Ordering::Release);
    }

    /// Polls all currently ready tasks.
    pub fn poll_ready(&self) -> usize {
        let mut polled = 0;
        while let Some(task) = self.inner.ready.lock().pop_front() {
            task.queued.store(false, Ordering::Release);
            task.poll();
            polled += 1;
        }
        polled
    }

    /// Returns whether the executor has ready tasks.
    pub fn has_ready_tasks(&self) -> bool {
        !self.inner.ready.lock().is_empty()
    }

    /// Returns whether at least one local task is still alive.
    pub fn has_live_tasks(&self) -> bool {
        self.inner.task_count.load(Ordering::Acquire) != 0
    }
}

/// Handle used to spawn futures onto a [`LocalExecutorCore`].
#[derive(Clone)]
pub struct LocalSpawnerCore {
    inner: Arc<LocalExecutorInner>,
    _not_send: PhantomData<Rc<()>>,
}

impl LocalSpawnerCore {
    /// Spawns a future on the local executor.
    pub fn spawn_local<F>(&self, future: F) -> Result<(), SpawnLocalError>
    where
        F: Future<Output = ()> + 'static,
    {
        let task = Arc::new(LocalTask {
            future: SpinMutex::new(Some(Box::pin(future))),
            queued: AtomicBool::new(true),
            completed: AtomicBool::new(false),
            executor: Arc::downgrade(&self.inner),
        });
        self.inner.task_count.fetch_add(1, Ordering::AcqRel);
        self.inner.ready.lock().push_back(task);
        (self.inner.pend)();
        Ok(())
    }
}

/// Error returned by [`LocalSpawnerCore::spawn_local`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnLocalError;

#[cfg(test)]
mod tests {
    use alloc::{rc::Rc, sync::Arc};
    use core::{
        cell::Cell,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::LocalExecutorCore;

    #[test]
    fn local_executor_polls_spawned_future_and_pends_host() {
        let pend_count = Arc::new(AtomicUsize::new(0));
        let pend_count_for_closure = pend_count.clone();
        let executor = LocalExecutorCore::new(Arc::new(move || {
            pend_count_for_closure.fetch_add(1, Ordering::AcqRel);
        }));
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_for_future = ran.clone();

        executor
            .spawner()
            .spawn_local(async move {
                ran_for_future.fetch_add(1, Ordering::AcqRel);
            })
            .unwrap();

        assert_eq!(pend_count.load(Ordering::Acquire), 1);
        executor.enter();
        assert_eq!(executor.poll_ready(), 1);
        executor.leave();
        assert_eq!(ran.load(Ordering::Acquire), 1);
        assert!(!executor.has_live_tasks());
    }

    #[test]
    fn local_executor_accepts_non_send_future() {
        let executor = LocalExecutorCore::new(Arc::new(|| {}));
        let value = Rc::new(Cell::new(0));
        let value_for_future = value.clone();

        executor
            .spawner()
            .spawn_local(async move {
                value_for_future.set(1);
            })
            .unwrap();

        executor.enter();
        assert_eq!(executor.poll_ready(), 1);
        executor.leave();

        assert_eq!(value.get(), 1);
        assert!(!executor.has_live_tasks());
    }
}
