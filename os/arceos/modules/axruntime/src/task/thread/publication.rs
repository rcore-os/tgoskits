//! Prepared-thread placement and staged activation transaction.

use super::*;

impl PreparedThread {
    /// Returns a strong handle for binding OS-owned identity before publication.
    pub fn thread_handle(&self) -> ThreadHandle {
        self.handle
            .as_ref()
            .expect("prepared thread was already consumed")
            .clone()
    }

    /// Places and immediately activates a thread with no external publication
    /// transaction.
    pub fn publish(self) -> Result<ThreadHandle, TaskError> {
        Ok(self.stage()?.activate())
    }

    /// Completes the fallible scheduler placement phase without entering the
    /// caller-owned thread entry point.
    ///
    /// The scheduler may select the staged thread, but its runtime trampoline
    /// remains blocked on an internal start gate. This lets an OS complete its
    /// public identity transaction before [`StagedThread::activate`] provides
    /// the final infallible release, matching Linux's `wake_up_new_task`
    /// boundary.
    pub fn stage(mut self) -> Result<StagedThread, TaskError> {
        let handle = self
            .handle
            .take()
            .expect("prepared thread was already consumed");
        publish_prepared_thread(self.system, handle).map(|handle| StagedThread {
            handle: Some(handle),
            start: Arc::clone(&self.start),
        })
    }

    pub(in crate::task) fn new(
        system: &'static TaskSystem,
        handle: ThreadHandle,
        start: Arc<RuntimeThreadStart>,
    ) -> Self {
        Self {
            system,
            handle: Some(handle),
            start,
        }
    }
}

impl StagedThread {
    /// Returns a strong handle for the OS publication transaction.
    pub fn thread_handle(&self) -> ThreadHandle {
        self.handle
            .as_ref()
            .expect("staged thread was already consumed")
            .clone()
    }

    /// Releases the staged thread to execute its caller-owned entry point.
    pub fn activate(mut self) -> ThreadHandle {
        let handle = self
            .handle
            .take()
            .expect("staged thread was already consumed");
        self.start.activate();
        handle
    }
}

impl Drop for StagedThread {
    fn drop(&mut self) {
        self.start.abort();
    }
}

impl Drop for PreparedThread {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            cleanup_failed_thread(self.system, handle);
        }
    }
}

impl RuntimeThreadStart {
    pub(in crate::task) const fn new() -> Self {
        Self {
            state: AtomicU8::new(THREAD_START_PENDING),
            wait: WaitQueue::new(),
        }
    }

    fn activate(&self) {
        self.state
            .compare_exchange(
                THREAD_START_PENDING,
                THREAD_START_ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .unwrap_or_else(|state| panic!("invalid staged-thread activation state: {state}"));
        self.wait.notify_all();
    }

    fn abort(&self) {
        if self
            .state
            .compare_exchange(
                THREAD_START_PENDING,
                THREAD_START_ABORTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.wait.notify_all();
        }
    }

    pub(in crate::task) fn wait_for_activation(&self) -> bool {
        match self.state.load(Ordering::Acquire) {
            THREAD_START_ACTIVE => return true,
            THREAD_START_ABORTED => return false,
            THREAD_START_PENDING => {}
            state => panic!("invalid runtime thread-start state: {state}"),
        }
        self.wait
            .wait_until(|| self.state.load(Ordering::Acquire) != THREAD_START_PENDING);
        match self.state.load(Ordering::Acquire) {
            THREAD_START_ACTIVE => true,
            THREAD_START_ABORTED => false,
            state => panic!("invalid completed thread-start state: {state}"),
        }
    }
}

fn publish_prepared_thread(
    system: &'static TaskSystem,
    handle: ThreadHandle,
) -> Result<ThreadHandle, TaskError> {
    let result = system.make_ready(handle.id()).and_then(|()| {
        with_current_cpu_local_mut_owner(|cpu| system.place_ready(cpu, handle.id()))
    });
    if let Err(error) = result {
        cleanup_failed_thread(system, handle);
        return Err(error);
    }
    Ok(handle)
}

fn cleanup_failed_thread(system: &TaskSystem, handle: ThreadHandle) {
    let thread = handle.id();
    let _ = system.mark_exited(thread);
    drop(handle);
    let _ = system.reap_thread(thread);
}
