#[track_caller]
pub fn ax_sleep_until(deadline: crate::time::AxTimeValue) {
    #[cfg(feature = "multitask")]
    ax_runtime::task::sleep_until(deadline);
    #[cfg(not(feature = "multitask"))]
    ax_hal::time::busy_wait_until(deadline);
}

#[track_caller]
pub fn ax_yield_now() {
    #[cfg(feature = "multitask")]
    if let Err(error) = ax_runtime::task::yield_current_cpu() {
        panic!("ax_yield_now failed at a scheduler safe point: {error}");
    }
    #[cfg(not(feature = "multitask"))]
    if cfg!(feature = "irq") {
        ax_hal::asm::wait_for_irqs();
    } else {
        core::hint::spin_loop();
    }
}

#[track_caller]
pub fn ax_exit(exit_code: i32) -> ! {
    #[cfg(feature = "multitask")]
    ax_runtime::task::exit_current(exit_code);
    #[cfg(not(feature = "multitask"))]
    {
        let _ = exit_code;
        crate::sys::ax_terminate();
    }
}

cfg_task! {
    use core::time::Duration;
    use ax_runtime::task::{CpuId, CpuSet};

    /// A handle to a task.
    pub struct AxTaskHandle {
        inner: ax_runtime::task::ThreadHandle,
        id: u64,
    }

    impl AxTaskHandle {
        /// Returns the task ID.
        pub fn id(&self) -> u64 {
            self.id
        }
    }

    /// A mask to specify the CPU affinity.
    pub type AxCpuMask = ax_cpumask::CpuMask<{ ax_runtime::CPU_CAPACITY }>;

    pub use ax_sync::RawMutex as AxRawMutex;

    /// A handle to a wait queue.
    ///
    /// A wait queue is used to store sleeping tasks waiting for a certain event
    /// to happen.
    pub struct AxWaitQueueHandle(ax_runtime::task::WaitQueue);

    impl AxWaitQueueHandle {
        /// Creates a new empty wait queue.
        pub const fn new() -> Self {
            Self(ax_runtime::task::WaitQueue::new())
        }
    }

    impl Default for AxWaitQueueHandle {
        fn default() -> Self {
            Self::new()
        }
    }

    pub fn ax_current_task_id() -> u64 {
        ax_runtime::task::current_thread_id()
            .unwrap_or_else(|error| panic!("current task is unavailable: {error}"))
            .as_u64()
    }

    pub fn ax_spawn<F>(f: F, name: alloc::string::String, stack_size: usize) -> AxTaskHandle
    where
        F: FnOnce() + Send + 'static,
    {
        let inner = ax_runtime::task::spawn_raw(f, name, stack_size)
            .unwrap_or_else(|error| panic!("failed to spawn task: {error}"));
        AxTaskHandle {
            id: inner.id().as_u64(),
            inner,
        }
    }

    #[track_caller]
    pub fn ax_wait_for_exit(task: AxTaskHandle) -> i32 {
        ax_runtime::task::join_thread(task.inner)
            .unwrap_or_else(|error| panic!("failed to join task: {error}"))
    }

    pub fn ax_set_current_priority(prio: isize) -> crate::AxResult {
        use ax_runtime::task::{Nice, SchedulePolicy};

        let nice = i8::try_from(prio)
            .ok()
            .and_then(|value| Nice::new(value).ok())
            .ok_or(crate::AxError::InvalidInput)?;
        let thread = task_result(
            ax_runtime::task::current_thread_id(),
            "read current task identity",
        )?;
        let policy = task_result(
            ax_runtime::task::thread_policy(thread),
            "read current scheduling policy",
        )?;
        let SchedulePolicy::Fair { mode, .. } = policy else {
            return Err(crate::AxError::OperationNotSupported);
        };
        task_result(
            ax_runtime::task::set_thread_policy(thread, SchedulePolicy::fair(nice, mode)),
            "set current task priority",
        )
    }

    #[track_caller]
    pub fn ax_set_current_affinity(cpumask: AxCpuMask) -> crate::AxResult {
        let thread = task_result(
            ax_runtime::task::current_thread_id(),
            "read current task identity",
        )?;
        let topology_len = task_result(
            ax_runtime::task::thread_affinity(thread),
            "read task affinity topology",
        )?
        .topology_len();
        let affinity = cpu_set_from_mask(cpumask, topology_len)?;
        task_result(
            ax_runtime::task::set_current_thread_affinity(affinity),
            "set current task affinity",
        )
    }

    #[track_caller]
    pub fn ax_wait_queue_wait(wq: &AxWaitQueueHandle, timeout: Option<Duration>) -> bool {
        #[cfg(feature = "irq")]
        if let Some(dur) = timeout {
            return wq.0.wait_timeout(dur);
        }

        if timeout.is_some() {
            ax_log::warn!("ax_wait_queue_wait: the `timeout` argument is ignored without the `irq` feature");
        }
        wq.0.wait();
        false
    }

    #[track_caller]
    pub fn ax_wait_queue_wait_until(
        wq: &AxWaitQueueHandle,
        until_condition: impl Fn() -> bool,
        timeout: Option<Duration>,
    ) -> bool {
        #[cfg(feature = "irq")]
        if let Some(dur) = timeout {
            return wq.0.wait_timeout_until(dur, until_condition);
        }

        if timeout.is_some() {
            ax_log::warn!("ax_wait_queue_wait_until: the `timeout` argument is ignored without the `irq` feature");
        }
        wq.0.wait_until(until_condition);
        false
    }

    /// Blocks until `until_condition` becomes true or the absolute monotonic
    /// `deadline` elapses.
    ///
    /// Returns `true` only when the deadline wins.
    #[track_caller]
    pub fn ax_wait_queue_wait_until_deadline(
        wq: &AxWaitQueueHandle,
        deadline: Duration,
        until_condition: impl Fn() -> bool,
    ) -> bool {
        #[cfg(feature = "irq")]
        return wq.0.wait_until_deadline(deadline, until_condition);

        #[cfg(not(feature = "irq"))]
        {
            let _ = deadline;
            ax_log::warn!(
                "ax_wait_queue_wait_until_deadline: the deadline is ignored without the `irq` feature"
            );
            wq.0.wait_until(until_condition);
            false
        }
    }

    pub fn ax_wait_queue_wake(wq: &AxWaitQueueHandle, count: u32) {
        if count == u32::MAX {
            wq.0.notify_all();
        } else {
            for _ in 0..count {
                if !wq.0.notify_one() {
                    break;
                }
            }
        }
    }

    pub fn ax_wait_queue_wake_one_with<F>(wq: &AxWaitQueueHandle, func: F)
    where
        F: Fn(u64),
    {
        wq.0.notify_one_with(func);
    }

    fn task_result<T>(
        result: Result<T, ax_runtime::task::TaskError>,
        operation: &'static str,
    ) -> crate::AxResult<T> {
        result.map_err(|error| {
            ax_log::warn!("{operation} failed: {error}");
            map_task_error(error)
        })
    }

    fn map_task_error(error: ax_runtime::task::TaskError) -> crate::AxError {
        use ax_runtime::task::TaskError;

        match error {
            TaskError::InvalidConfiguration
            | TaskError::InvalidCpuCount(_)
            | TaskError::InvalidCpu(_)
            | TaskError::InvalidNice(_)
            | TaskError::InvalidRtPriority(_)
            | TaskError::InvalidRoundRobinQuantum
            | TaskError::InvalidDeadline { .. }
            | TaskError::UnsupportedDeadlineFlags(_) => crate::AxError::InvalidInput,
            TaskError::DeadlineAdmission
            | TaskError::DeadlineAffinity
            | TaskError::ActiveTimerAffinity
            | TaskError::ThreadBusy
            | TaskError::ThreadPinned => crate::AxError::ResourceBusy,
            TaskError::TimerCapacity => crate::AxError::NoMemory,
            TaskError::UnsafeContext => crate::AxError::OperationNotPermitted,
            TaskError::StaleThreadId => crate::AxError::NotFound,
            TaskError::NotInitialized
            | TaskError::InvalidRuntimeHandle
            | TaskError::CpuOwnerBorrowed
            | TaskError::CpuOwnerMismatch { .. }
            | TaskError::ExecutorOwnerMismatch { .. }
            | TaskError::CpuAlreadyOnline(_)
            | TaskError::CpuOffline(_)
            | TaskError::InvalidTransition { .. }
            | TaskError::AlreadyQueued
            | TaskError::NotReady
            | TaskError::NotExited
            | TaskError::NoRunnableThread
            | TaskError::InvalidPiState
            | TaskError::PiCycle
            | TaskError::RuntimeFailure(_) => crate::AxError::BadState,
        }
    }

    fn cpu_set_from_mask(cpumask: AxCpuMask, topology_len: usize) -> crate::AxResult<CpuSet> {
        if cpumask.is_empty() {
            return Err(crate::AxError::InvalidInput);
        }
        let mut affinity = CpuSet::empty(topology_len);
        for cpu_index in &cpumask {
            let cpu_index = u32::try_from(cpu_index).map_err(|_| crate::AxError::InvalidInput)?;
            if !affinity.insert(CpuId::new(cpu_index)) {
                return Err(crate::AxError::InvalidInput);
            }
        }
        Ok(affinity)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cpu_mask_conversion_preserves_allowed_cpu() {
            let affinity = cpu_set_from_mask(AxCpuMask::one_shot(0), 1).unwrap();

            assert!(affinity.contains(CpuId::new(0)));
        }

        #[test]
        fn cpu_mask_conversion_rejects_empty_mask() {
            assert_eq!(
                cpu_set_from_mask(AxCpuMask::new(), 1),
                Err(crate::AxError::InvalidInput)
            );
        }

        #[test]
        fn cpu_mask_conversion_rejects_cpu_outside_topology() {
            assert_eq!(
                cpu_set_from_mask(AxCpuMask::one_shot(0), 0),
                Err(crate::AxError::InvalidInput)
            );
        }
    }
}
