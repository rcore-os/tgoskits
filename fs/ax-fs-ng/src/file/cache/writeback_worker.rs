use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use super::reclaim;
use crate::os::{
    BlockNotification, BlockRuntimeOps, BlockThread, monotonic_time, runtime_ops,
    sync::SleepMutex as Mutex,
};

const PERIODIC_WRITEBACK_INTERVAL: Duration = Duration::from_secs(30);

struct PeriodicDeadline {
    next: Duration,
    interval: Duration,
}

impl PeriodicDeadline {
    fn new(now: Duration, interval: Duration) -> Self {
        Self {
            next: now.saturating_add(interval),
            interval,
        }
    }

    fn remaining(&self, now: Duration) -> Duration {
        self.next.saturating_sub(now)
    }

    fn take_due(&mut self, now: Duration) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now.saturating_add(self.interval);
        true
    }
}

fn run_ready_work(
    work: &PendingWork,
    deadline: &mut PeriodicDeadline,
    mut scan: impl FnMut(),
    mut periodic_scan: impl FnMut(),
    mut now: impl FnMut() -> Duration,
) {
    while work.take() {
        scan();
        if deadline.take_due(now()) {
            periodic_scan();
        }
    }
    if deadline.take_due(now()) {
        periodic_scan();
    }
}

struct PendingWork {
    pending: AtomicBool,
}

impl PendingWork {
    const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    fn publish(&self) {
        self.pending.store(true, Ordering::Release);
    }

    fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    #[cfg(test)]
    fn run_pending(&self, mut scan: impl FnMut()) {
        while self.take() {
            scan();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerState {
    Idle,
    Starting,
    Running,
}

struct WorkerStartup {
    state: Mutex<WorkerState>,
}

impl WorkerStartup {
    const fn new() -> Self {
        Self {
            state: Mutex::new(WorkerState::Idle),
        }
    }

    fn ensure_running(&self, start: impl FnOnce() -> bool) -> bool {
        let mut state = self.state.lock();
        if *state == WorkerState::Running {
            return true;
        }

        *state = WorkerState::Starting;
        if start() {
            *state = WorkerState::Running;
            true
        } else {
            *state = WorkerState::Idle;
            false
        }
    }

    #[cfg(test)]
    fn is_running(&self) -> bool {
        *self.state.lock() == WorkerState::Running
    }
}

struct BackgroundWritebackManager {
    startup: WorkerStartup,
    work: PendingWork,
    notification: Mutex<Option<Arc<dyn BlockNotification>>>,
    _thread: Mutex<Option<Box<dyn BlockThread>>>,
    scan: fn(),
    periodic_scan: fn(),
}

impl BackgroundWritebackManager {
    const fn new() -> Self {
        Self::new_with_scans(scan_requested_files, scan_all_registered_files)
    }

    #[cfg(test)]
    const fn new_with_scan(scan: fn()) -> Self {
        Self::new_with_scans(scan, || {})
    }

    const fn new_with_scans(scan: fn(), periodic_scan: fn()) -> Self {
        Self {
            startup: WorkerStartup::new(),
            work: PendingWork::new(),
            notification: Mutex::new(None),
            _thread: Mutex::new(None),
            scan,
            periodic_scan,
        }
    }

    fn ensure_running_with_runtime(&'static self, runtime: &'static dyn BlockRuntimeOps) -> bool {
        self.startup.ensure_running(|| self.start(runtime))
    }

    fn request(&'static self) -> bool {
        let Ok(runtime) = runtime_ops() else {
            return false;
        };
        self.request_with_runtime(runtime)
    }

    fn request_with_runtime(&'static self, runtime: &'static dyn BlockRuntimeOps) -> bool {
        self.work.publish();
        let running = self.ensure_running_with_runtime(runtime);
        if running && let Some(notification) = self.notification.lock().as_ref().cloned() {
            notification.notify();
        }
        running
    }

    fn start(&'static self, runtime: &'static dyn BlockRuntimeOps) -> bool {
        let online_cpus = runtime.online_cpu_count();
        if online_cpus == 0 {
            return false;
        }
        let notification = runtime.notification();
        let worker_notification = Arc::clone(&notification);
        let cpu = runtime.current_cpu() % online_cpus;
        let entry = Box::new(move || self.run(worker_notification));
        let Ok(thread) = runtime.spawn_pinned(String::from("axfs-writeback"), cpu, entry) else {
            return false;
        };

        *self.notification.lock() = Some(notification);
        *self._thread.lock() = Some(thread);
        true
    }

    fn run(&'static self, notification: Arc<dyn BlockNotification>) {
        let mut deadline = PeriodicDeadline::new(monotonic_time(), PERIODIC_WRITEBACK_INTERVAL);
        loop {
            notification.wait_timeout(deadline.remaining(monotonic_time()));
            run_ready_work(
                &self.work,
                &mut deadline,
                self.scan,
                self.periodic_scan,
                monotonic_time,
            );
        }
    }

    #[cfg(test)]
    fn has_published_resources(&self) -> bool {
        self.notification.lock().is_some() || self._thread.lock().is_some()
    }
}

static BACKGROUND_WRITEBACK: ax_lazyinit::LazyLock<BackgroundWritebackManager> =
    ax_lazyinit::LazyLock::new(BackgroundWritebackManager::new);

pub(super) fn request_background_writeback() -> bool {
    #[cfg(test)]
    if let Some(result) = tests::forced_worker_result() {
        return result;
    }
    BACKGROUND_WRITEBACK.request()
}

pub(crate) fn start_background_writeback(runtime: &'static dyn BlockRuntimeOps) -> bool {
    BACKGROUND_WRITEBACK.ensure_running_with_runtime(runtime)
}

#[cfg(test)]
pub(super) fn request_background_writeback_with_runtime(
    runtime: &'static dyn BlockRuntimeOps,
) -> bool {
    BACKGROUND_WRITEBACK.request_with_runtime(runtime)
}

fn scan_requested_files() {
    for file in reclaim::take_background_writeback_files() {
        if let Err(error) = file.writeback_dirty_for_background() {
            warn!("background file writeback failed: {error:?}");
        }
    }
}

pub(super) fn scan_all_registered_files() {
    for file in reclaim::snapshot_cached_files() {
        if let Err(error) = file.writeback_dirty_for_periodic() {
            warn!("periodic file writeback failed: {error:?}");
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use core::{cell::Cell, time::Duration};
    use std::{
        sync::{Arc, Condvar, Mutex, mpsc},
        thread,
    };

    use super::{
        BackgroundWritebackManager, BlockNotification, BlockRuntimeOps, BlockThread, PendingWork,
        PeriodicDeadline, WorkerStartup, run_ready_work,
    };
    use crate::{BlockError, BlockResult};

    std::thread_local! {
        static FORCED_WORKER_RESULT: Cell<Option<bool>> = const { Cell::new(None) };
    }

    pub(super) fn forced_worker_result() -> Option<bool> {
        FORCED_WORKER_RESULT.with(Cell::get)
    }

    pub(crate) fn with_forced_worker_result<R>(result: bool, run: impl FnOnce() -> R) -> R {
        FORCED_WORKER_RESULT.with(|forced| {
            let previous = forced.replace(Some(result));
            let output = run();
            forced.set(previous);
            output
        })
    }

    #[test]
    fn work_published_during_scan_runs_another_scan() {
        let work = PendingWork::new();
        work.publish();
        let mut scans = 0;

        work.run_pending(|| {
            scans += 1;
            if scans == 1 {
                work.publish();
            }
        });

        assert_eq!(scans, 2);
        assert!(!work.take());
    }

    #[test]
    fn repeated_immediate_work_cannot_starve_periodic_work() {
        let work = PendingWork::new();
        let mut deadline = PeriodicDeadline::new(Duration::ZERO, Duration::from_secs(30));
        let scans = Cell::new(0);
        let periodic_scans = Cell::new(0);
        let current_time = Cell::new(Duration::ZERO);
        work.publish();

        run_ready_work(
            &work,
            &mut deadline,
            || {
                scans.set(scans.get() + 1);
                if scans.get() < 3 {
                    work.publish();
                }
            },
            || periodic_scans.set(periodic_scans.get() + 1),
            || {
                let next = current_time.get() + Duration::from_secs(10);
                current_time.set(next);
                next
            },
        );

        assert_eq!(scans.get(), 3);
        assert_eq!(periodic_scans.get(), 1);
    }

    #[test]
    fn concurrent_successful_requests_start_one_worker() {
        let startup = Arc::new(WorkerStartup::new());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = Arc::clone(&startup);
        let first_join = thread::spawn(move || {
            first.ensure_running(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                true
            })
        });
        entered_rx.recv().unwrap();

        let second = Arc::clone(&startup);
        let second_join = thread::spawn(move || second.ensure_running(|| panic!("second spawn")));
        release_tx.send(()).unwrap();

        assert!(first_join.join().unwrap());
        assert!(second_join.join().unwrap());
        assert!(startup.is_running());
    }

    #[test]
    fn publisher_arriving_during_failed_start_retries_after_rollback() {
        let startup = Arc::new(WorkerStartup::new());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = Arc::clone(&startup);
        let first_join = thread::spawn(move || {
            first.ensure_running(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                false
            })
        });
        entered_rx.recv().unwrap();

        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let second = Arc::clone(&startup);
        let second_join = thread::spawn(move || {
            second_entered_tx.send(()).unwrap();
            second.ensure_running(|| true)
        });
        second_entered_rx.recv().unwrap();
        release_tx.send(()).unwrap();

        assert!(!first_join.join().unwrap());
        assert!(second_join.join().unwrap());
        assert!(startup.is_running());
    }

    struct TestNotification {
        pending: Mutex<bool>,
        ready: Condvar,
    }

    impl TestNotification {
        const fn new() -> Self {
            Self {
                pending: Mutex::new(false),
                ready: Condvar::new(),
            }
        }
    }

    impl BlockNotification for TestNotification {
        fn notify(&self) {
            *self.pending.lock().unwrap() = true;
            self.ready.notify_one();
        }

        fn wait(&self) {
            let mut pending = self.pending.lock().unwrap();
            while !*pending {
                pending = self.ready.wait(pending).unwrap();
            }
            *pending = false;
        }

        fn wait_timeout(&self, duration: core::time::Duration) -> bool {
            let mut pending = self.pending.lock().unwrap();
            if *pending {
                *pending = false;
                return false;
            }
            let (mut pending, timeout) = self.ready.wait_timeout(pending, duration).unwrap();
            if *pending {
                *pending = false;
                false
            } else {
                timeout.timed_out()
            }
        }
    }

    struct DetachedThread;

    impl BlockThread for DetachedThread {
        fn join(&self) {}
    }

    struct FailingRuntime;

    impl BlockRuntimeOps for FailingRuntime {
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
            _name: alloc::string::String,
            _cpu: usize,
            _entry: alloc::boxed::Box<dyn FnOnce() + Send + 'static>,
        ) -> BlockResult<alloc::boxed::Box<dyn BlockThread>> {
            Err(BlockError::RuntimeUnavailable)
        }
    }

    struct ThreadRuntime;

    impl BlockRuntimeOps for ThreadRuntime {
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
            _name: alloc::string::String,
            _cpu: usize,
            entry: alloc::boxed::Box<dyn FnOnce() + Send + 'static>,
        ) -> BlockResult<alloc::boxed::Box<dyn BlockThread>> {
            thread::spawn(entry);
            Ok(alloc::boxed::Box::new(DetachedThread))
        }
    }

    static FAILING_RUNTIME: FailingRuntime = FailingRuntime;
    static THREAD_RUNTIME: ThreadRuntime = ThreadRuntime;

    pub(crate) fn thread_runtime() -> &'static dyn BlockRuntimeOps {
        &THREAD_RUNTIME
    }

    #[test]
    fn spawn_failure_leaves_no_published_manager_resources_and_can_retry() {
        let manager = alloc::boxed::Box::leak(alloc::boxed::Box::new(
            BackgroundWritebackManager::new_with_scan(|| {}),
        ));

        assert!(!manager.request_with_runtime(&FAILING_RUNTIME));
        assert!(!manager.startup.is_running());
        assert!(!manager.has_published_resources());
        assert!(manager.request_with_runtime(&THREAD_RUNTIME));
        assert!(manager.startup.is_running());
        assert!(manager.has_published_resources());
    }
}
