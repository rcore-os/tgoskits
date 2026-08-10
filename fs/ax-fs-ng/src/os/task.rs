use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_sync::SpinRwLock as RwLock;

/// Wait/notify object created and owned by the block runtime.
pub trait BlockNotification: Send + Sync + 'static {
    /// Publishes work from normal task context.
    fn notify(&self);

    /// Publishes work from hard IRQ context without allocation or sleeping.
    fn notify_from_irq(&self);

    /// Blocks until a notification is pending and consumes that notification.
    #[track_caller]
    fn wait(&self);

    /// Blocks until notified or the duration expires.
    ///
    /// Returns `true` when the wait timed out.
    #[track_caller]
    fn wait_timeout(&self, duration: Duration) -> bool;
}

/// Join token for one block maintenance task.
pub trait BlockThread: Send + Sync + 'static {
    /// Waits for the maintenance task to exit.
    fn join(&self);
}

/// Scheduler and CPU-affinity capabilities consumed by the block runtime.
pub trait BlockRuntimeOps: Send + Sync {
    /// Returns the logical CPU executing the caller.
    fn current_cpu(&self) -> usize;

    /// Returns the number of CPUs whose scheduler, IPI, and local IRQ path are
    /// fully online.
    fn online_cpu_count(&self) -> usize;

    /// Returns whether the current context may block.
    fn can_block(&self) -> bool;

    /// Creates an independent lost-wakeup-safe wait/notify object.
    fn notification(&self) -> Arc<dyn BlockNotification>;

    /// Starts a maintenance task and binds it to one online CPU.
    ///
    /// # Errors
    ///
    /// Returns an error when the task cannot be created or the requested CPU
    /// cannot be used. On error, `entry` has not run.
    fn spawn_pinned(
        &self,
        name: String,
        cpu: usize,
        entry: Box<dyn FnOnce() + Send + 'static>,
    ) -> AxResult<Box<dyn BlockThread>>;
}

static RUNTIME_OPS: RwLock<Option<&'static dyn BlockRuntimeOps>> = RwLock::new(None);
static RUNTIME_READY: AtomicBool = AtomicBool::new(false);

/// Installs the runtime task capability implementation.
pub fn set_runtime_ops(ops: &'static dyn BlockRuntimeOps) {
    *RUNTIME_OPS.write() = Some(ops);
    RUNTIME_READY.store(true, Ordering::Release);
}

/// Returns the installed block runtime capabilities.
///
/// # Errors
///
/// Returns [`AxError::BadState`] before `axruntime` installs the adapter.
pub fn runtime_ops() -> AxResult<&'static dyn BlockRuntimeOps> {
    RUNTIME_OPS
        .read()
        .as_ref()
        .copied()
        .ok_or(AxError::BadState)
}

/// Returns whether the runtime adapter has been installed.
pub fn has_runtime_ops() -> bool {
    RUNTIME_READY.load(Ordering::Acquire)
}

#[cfg(test)]
pub(crate) fn install_test_runtime_ops() {
    set_runtime_ops(&tests::TEST_RUNTIME_OPS);
    crate::os::time::set_time_provider(&tests::TEST_TIME_PROVIDER);
}

#[cfg(test)]
pub(crate) fn reset_test_wait_timeout_count() {
    tests::TEST_WAIT_TIMEOUTS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn test_wait_timeout_count() -> usize {
    tests::TEST_WAIT_TIMEOUTS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, string::String, sync::Arc};
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use std::{
        sync::{Condvar, Mutex, OnceLock},
        thread::{self, JoinHandle},
        time::Instant,
    };

    use ax_errno::AxResult;

    use super::{BlockNotification, BlockRuntimeOps, BlockThread};
    use crate::os::time::BlockTimeProvider;

    pub(super) static TEST_RUNTIME_OPS: TestRuntimeOps = TestRuntimeOps;
    pub(super) static TEST_TIME_PROVIDER: TestTimeProvider = TestTimeProvider;
    pub(super) static TEST_WAIT_TIMEOUTS: AtomicUsize = AtomicUsize::new(0);
    static TEST_START: OnceLock<Instant> = OnceLock::new();

    pub(super) struct TestRuntimeOps;
    pub(super) struct TestTimeProvider;

    struct TestNotification {
        pending: Mutex<bool>,
        ready: Condvar,
    }

    struct TestThread {
        join: Mutex<Option<JoinHandle<()>>>,
    }

    impl TestNotification {
        const fn new() -> Self {
            Self {
                pending: Mutex::new(false),
                ready: Condvar::new(),
            }
        }

        fn publish(&self) {
            *self.pending.lock().unwrap() = true;
            self.ready.notify_one();
        }
    }

    impl BlockNotification for TestNotification {
        fn notify(&self) {
            self.publish();
        }

        fn notify_from_irq(&self) {
            self.publish();
        }

        #[track_caller]
        fn wait(&self) {
            let mut pending = self.pending.lock().unwrap();
            while !*pending {
                pending = self.ready.wait(pending).unwrap();
            }
            *pending = false;
        }

        #[track_caller]
        fn wait_timeout(&self, duration: Duration) -> bool {
            TEST_WAIT_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
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

    impl BlockThread for TestThread {
        fn join(&self) {
            if let Some(join) = self.join.lock().unwrap().take() {
                join.join().unwrap();
            }
        }
    }

    impl BlockRuntimeOps for TestRuntimeOps {
        fn current_cpu(&self) -> usize {
            0
        }

        fn online_cpu_count(&self) -> usize {
            1
        }

        fn can_block(&self) -> bool {
            true
        }

        fn notification(&self) -> Arc<dyn BlockNotification> {
            Arc::new(TestNotification::new())
        }

        fn spawn_pinned(
            &self,
            name: String,
            _cpu: usize,
            entry: Box<dyn FnOnce() + Send + 'static>,
        ) -> AxResult<Box<dyn BlockThread>> {
            let join = thread::Builder::new().name(name).spawn(entry).unwrap();
            Ok(Box::new(TestThread {
                join: Mutex::new(Some(join)),
            }))
        }
    }

    impl BlockTimeProvider for TestTimeProvider {
        fn wall_time(&self) -> Duration {
            TEST_START.get_or_init(Instant::now).elapsed()
        }
    }
}
