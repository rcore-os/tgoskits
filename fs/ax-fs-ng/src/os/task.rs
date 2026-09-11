use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_sync::SpinRwLock as RwLock;

use crate::{BlockError, BlockResult};

/// Wait/notify object created and owned by the block runtime.
pub trait BlockNotification: Send + Sync + 'static {
    /// Publishes work from task or hard IRQ context without allocation or
    /// sleeping.
    fn notify(&self);

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
    ) -> BlockResult<Box<dyn BlockThread>>;
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
/// Returns [`BlockError::RuntimeUnavailable`] before `axruntime` installs the adapter.
pub fn runtime_ops() -> BlockResult<&'static dyn BlockRuntimeOps> {
    RUNTIME_OPS
        .read()
        .as_ref()
        .copied()
        .ok_or(BlockError::RuntimeUnavailable)
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

/// Temporarily overrides the host test runtime's blocking capability for the
/// current thread.
#[cfg(test)]
pub(crate) fn test_can_block(can_block: bool) -> TestCanBlockGuard {
    TestCanBlockGuard {
        previous: tests::set_can_block(can_block),
    }
}

#[cfg(test)]
pub(crate) struct TestCanBlockGuard {
    previous: bool,
}

#[cfg(test)]
impl Drop for TestCanBlockGuard {
    fn drop(&mut self) {
        tests::set_can_block(self.previous);
    }
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
        cell::Cell,
        sync::{Condvar, Mutex, OnceLock},
        thread::{self, JoinHandle},
        time::Instant,
    };

    use super::{BlockNotification, BlockRuntimeOps, BlockThread};
    use crate::{BlockResult, os::time::BlockTimeProvider};

    pub(super) static TEST_RUNTIME_OPS: TestRuntimeOps = TestRuntimeOps;
    pub(super) static TEST_TIME_PROVIDER: TestTimeProvider = TestTimeProvider;
    pub(super) static TEST_WAIT_TIMEOUTS: AtomicUsize = AtomicUsize::new(0);
    static TEST_START: OnceLock<Instant> = OnceLock::new();

    std::thread_local! {
        static TEST_CAN_BLOCK: Cell<bool> = const { Cell::new(true) };
    }

    pub(super) struct TestRuntimeOps;
    pub(super) struct TestTimeProvider;

    pub(super) fn set_can_block(can_block: bool) -> bool {
        TEST_CAN_BLOCK.with(|value| value.replace(can_block))
    }

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

        #[track_caller]
        fn wait(&self) {
            assert!(
                TEST_CAN_BLOCK.with(Cell::get),
                "test runtime wait was called from a nonblocking context"
            );
            assert!(
                !crate::os::sync::current_thread_holds_irq_mutex(),
                "block notification wait cannot hold a non-sleeping lock"
            );
            let mut pending = self.pending.lock().unwrap();
            while !*pending {
                pending = self.ready.wait(pending).unwrap();
            }
            *pending = false;
        }

        #[track_caller]
        fn wait_timeout(&self, duration: Duration) -> bool {
            assert!(
                TEST_CAN_BLOCK.with(Cell::get),
                "test runtime timed wait was called from a nonblocking context"
            );
            assert!(
                !crate::os::sync::current_thread_holds_irq_mutex(),
                "block notification wait cannot hold a non-sleeping lock"
            );
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
            TEST_CAN_BLOCK.with(Cell::get)
        }

        fn notification(&self) -> Arc<dyn BlockNotification> {
            Arc::new(TestNotification::new())
        }

        fn spawn_pinned(
            &self,
            name: String,
            _cpu: usize,
            entry: Box<dyn FnOnce() + Send + 'static>,
        ) -> BlockResult<Box<dyn BlockThread>> {
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

        fn monotonic_time(&self) -> Duration {
            TEST_START.get_or_init(Instant::now).elapsed()
        }
    }
}
