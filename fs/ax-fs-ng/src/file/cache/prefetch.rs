//! Bounded speculative I/O. Demand uses the same validated fill ownership.

use alloc::{boxed::Box, collections::VecDeque, string::String, sync::Arc};

use super::{CachedFile, readahead::PrefetchRequest};
use crate::{
    BlockError, BlockResult,
    os::{BlockNotification, BlockRuntimeOps, BlockThread, runtime_ops, sync::Mutex},
};

const MAX_QUEUED_WINDOWS: usize = 16;

struct PrefetchJob {
    file: CachedFile,
    attempt: PrefetchRequest,
}

impl PrefetchJob {
    fn run(self) {
        let Ok(first_page) = u32::try_from(self.attempt.pages.start) else {
            return;
        };
        let Some(window_pages) = self
            .attempt
            .pages
            .end
            .checked_sub(self.attempt.pages.start)
            .filter(|count| *count != 0)
            .and_then(|count| usize::try_from(count).ok())
        else {
            return;
        };
        // Disk retention targets start at 512 pages, above two maximum
        // windows. Admission and publication retain their existing limits;
        // no cache lock is needed before entering the backing-read path.
        match self.file.prefetch_page_window(first_page, window_pages) {
            Ok(true) => self.file.readahead.lock().prefetched(&self.attempt),
            Ok(false) => {}
            Err(error) => {
                // Demand retries backing I/O and reports any real failure.
                log::debug!("file readahead deferred: {error:?}");
            }
        }
    }
}

struct WorkerResources {
    notification: Arc<dyn BlockNotification>,
    _thread: Box<dyn BlockThread>,
}

struct PrefetchWorker {
    queue: Mutex<VecDeque<PrefetchJob>>,
    resources: Mutex<Option<WorkerResources>>,
}

impl PrefetchWorker {
    const fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            resources: Mutex::new(None),
        }
    }

    fn start(
        &'static self,
        runtime: &'static dyn BlockRuntimeOps,
    ) -> BlockResult<Arc<dyn BlockNotification>> {
        let mut resources = self.resources.lock();
        if let Some(resources) = resources.as_ref() {
            return Ok(Arc::clone(&resources.notification));
        }
        let online_cpus = runtime.online_cpu_count();
        if online_cpus == 0 {
            return Err(BlockError::RuntimeUnavailable);
        }
        let notification = runtime.notification();
        let waiter = Arc::clone(&notification);
        let entry = Box::new(move || {
            loop {
                waiter.wait();
                loop {
                    let job = self.queue.lock().pop_front();
                    let Some(job) = job else { break };
                    job.run();
                }
            }
        });
        let cpu = runtime.current_cpu() % online_cpus;
        let thread = runtime.spawn_pinned(String::from("axfs-readahead"), cpu, entry)?;
        *resources = Some(WorkerResources {
            notification: Arc::clone(&notification),
            _thread: thread,
        });
        Ok(notification)
    }

    fn push(&self, job: PrefetchJob) -> bool {
        let mut queue = self.queue.lock();
        if queue.len() == MAX_QUEUED_WINDOWS || queue.try_reserve(1).is_err() {
            return false;
        }
        queue.push_back(job);
        true
    }

    fn request(&'static self, job: PrefetchJob, runtime: &'static dyn BlockRuntimeOps) {
        // Failed startup publishes no resources, so a later request can retry.
        // Dropping this speculative window does not change the completed read.
        let Ok(notification) = self.start(runtime) else {
            return;
        };
        if self.push(job) {
            notification.notify();
        }
    }
}

static PREFETCH: PrefetchWorker = PrefetchWorker::new();

pub(crate) fn start_background_readahead(runtime: &'static dyn BlockRuntimeOps) -> BlockResult<()> {
    PREFETCH.start(runtime).map(|_| ())
}

impl CachedFile {
    pub(super) fn prefetch_after_read(&self, number: u32) {
        if self.in_memory {
            return;
        }
        let Ok(runtime) = runtime_ops() else {
            return;
        };
        if !runtime.can_block() {
            return;
        }
        let pages = self.shared.len().div_ceil(super::PAGE_SIZE as u64);
        let Some(pages) = self.readahead.lock().take_prefetch(number, pages) else {
            return;
        };
        let job = PrefetchJob {
            file: self.clone(),
            attempt: pages,
        };
        #[cfg(not(test))]
        PREFETCH.request(job, runtime);
        #[cfg(test)]
        tests::enqueue(job);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use core::{
        cell::Cell,
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    use super::*;

    static WORKER: PrefetchWorker = PrefetchWorker::new();
    static EXCLUSIVE: Mutex<()> = Mutex::new(());
    static RUNTIME: QueueRuntime = QueueRuntime {
        state: Mutex::new(QueueRuntimeState {
            spawn_attempts: 0,
            notification: None,
        }),
    };

    std::thread_local! {
        static QUEUED: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn enqueue(job: PrefetchJob) {
        // Cache tests opt in to an isolated executor. Never start a global
        // host thread that could consume another test's page provider.
        QUEUED.with(|enabled| {
            if enabled.get() {
                WORKER.request(job, &RUNTIME);
            }
        });
    }

    struct QueueNotification {
        pending: AtomicBool,
    }

    impl BlockNotification for QueueNotification {
        fn notify(&self) {
            self.pending.store(true, Ordering::Release);
        }

        fn wait(&self) {
            panic!("the isolated executor drains jobs explicitly");
        }

        fn wait_timeout(&self, _duration: Duration) -> bool {
            panic!("the isolated executor drains jobs explicitly");
        }
    }

    struct QueuedThread {
        _entry: Mutex<Box<dyn FnOnce() + Send + 'static>>,
    }

    impl BlockThread for QueuedThread {
        fn join(&self) {
            panic!("the isolated executor does not run a host thread");
        }
    }

    #[derive(Default)]
    struct QueueRuntimeState {
        spawn_attempts: usize,
        notification: Option<Arc<QueueNotification>>,
    }

    struct QueueRuntime {
        state: Mutex<QueueRuntimeState>,
    }

    impl BlockRuntimeOps for QueueRuntime {
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
            let notification = Arc::new(QueueNotification {
                pending: AtomicBool::new(false),
            });
            self.state.lock().notification = Some(Arc::clone(&notification));
            notification
        }

        fn spawn_pinned(
            &self,
            _name: String,
            _cpu: usize,
            entry: Box<dyn FnOnce() + Send + 'static>,
        ) -> BlockResult<Box<dyn BlockThread>> {
            let mut state = self.state.lock();
            state.spawn_attempts += 1;
            if state.spawn_attempts == 1 {
                return Err(BlockError::NoMemory);
            }
            Ok(Box::new(QueuedThread {
                _entry: Mutex::new(entry),
            }))
        }
    }

    #[derive(Clone, Copy)]
    pub(in super::super) struct TestQueue;

    impl TestQueue {
        pub(in super::super) fn start(&self) -> BlockResult<()> {
            WORKER.start(&RUNTIME).map(|_| ())
        }

        pub(in super::super) fn spawn_attempts(&self) -> usize {
            RUNTIME.state.lock().spawn_attempts
        }

        pub(in super::super) fn run_one(&self) {
            let notification = RUNTIME.state.lock().notification.clone().unwrap();
            assert!(
                notification.pending.swap(false, Ordering::AcqRel),
                "queued readahead was not notified"
            );
            let job = WORKER
                .queue
                .lock()
                .pop_front()
                .expect("read did not queue readahead");
            if !WORKER.queue.lock().is_empty() {
                notification.notify();
            }
            job.run();
        }

        pub(in super::super) fn queued_len(&self) -> usize {
            WORKER.queue.lock().len()
        }
    }

    pub(in super::super) fn with_queue(run: impl FnOnce(&TestQueue)) {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                QUEUED.with(|enabled| enabled.set(false));
                let jobs = core::mem::take(&mut *WORKER.queue.lock());
                drop(jobs);
                let resources = WORKER.resources.lock().take();
                drop(resources);
                *RUNTIME.state.lock() = QueueRuntimeState::default();
            }
        }
        assert!(!QUEUED.with(Cell::get), "nested prefetch executor");
        let _exclusive = EXCLUSIVE.lock();
        QUEUED.with(|enabled| enabled.set(true));
        let _reset = Reset;
        run(&TestQueue);
    }
}
