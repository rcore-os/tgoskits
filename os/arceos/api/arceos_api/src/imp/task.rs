pub fn ax_sleep_until(deadline: crate::time::AxTimeValue) {
    #[cfg(feature = "multitask")]
    ax_task::sleep_until(deadline);
    #[cfg(not(feature = "multitask"))]
    ax_hal::time::busy_wait_until(deadline);
}

pub fn ax_yield_now() {
    #[cfg(feature = "multitask")]
    ax_task::yield_now();
    #[cfg(not(feature = "multitask"))]
    if cfg!(feature = "irq") {
        ax_hal::asm::wait_for_irqs();
    } else {
        core::hint::spin_loop();
    }
}

pub fn ax_exit(_exit_code: i32) -> ! {
    #[cfg(feature = "multitask")]
    ax_task::exit(_exit_code);
    #[cfg(not(feature = "multitask"))]
    crate::sys::ax_terminate();
}

cfg_task! {
    use core::time::Duration;

    /// A handle to a task.
    pub struct AxTaskHandle {
        inner: ax_task::AxTaskRef,
        id: u64,
    }

    impl AxTaskHandle {
        /// Returns the task ID.
        pub fn id(&self) -> u64 {
            self.id
        }
    }

    /// A mask to specify the CPU affinity.
    pub use ax_task::AxCpuMask;

    pub use ax_sync::RawMutex as AxRawMutex;

    /// A handle to a wait queue.
    ///
    /// A wait queue is used to store sleeping tasks waiting for a certain event
    /// to happen.
    pub struct AxWaitQueueHandle(ax_task::WaitQueue);

    impl AxWaitQueueHandle {
        /// Creates a new empty wait queue.
        pub const fn new() -> Self {
            Self(ax_task::WaitQueue::new())
        }
    }

    impl Default for AxWaitQueueHandle {
        fn default() -> Self {
            Self::new()
        }
    }

    pub fn ax_current_task_id() -> u64 {
        ax_task::current().id().as_u64()
    }

    pub fn ax_spawn<F>(f: F, name: alloc::string::String, stack_size: usize) -> AxTaskHandle
    where
        F: FnOnce() + Send + 'static,
    {
        let inner = ax_task::spawn_raw(f, name, stack_size);
        AxTaskHandle {
            id: inner.id().as_u64(),
            inner,
        }
    }

    pub fn ax_wait_for_exit(task: AxTaskHandle) -> i32 {
        task.inner.join()
    }

    pub fn ax_set_current_priority(prio: isize) -> crate::AxResult {
        if ax_task::set_priority(prio) {
            Ok(())
        } else {
            ax_errno::ax_err!(
                BadState,
                "ax_set_current_priority: failed to set task priority"
            )
        }
    }

    pub fn ax_set_current_affinity(cpumask: AxCpuMask) -> crate::AxResult {
        if ax_task::set_current_affinity(cpumask) {
            Ok(())
        } else {
            ax_errno::ax_err!(
                BadState,
                "ax_set_current_affinity: failed to set task affinity"
            )
        }
    }

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

    pub fn ax_wait_queue_wake(wq: &AxWaitQueueHandle, count: u32) {
        if count == u32::MAX {
            wq.0.notify_all(true);
        } else {
            for _ in 0..count {
                wq.0.notify_one(true);
            }
        }
    }

    pub fn ax_wait_queue_wake_one_with<F>(wq: &AxWaitQueueHandle, func: F)
    where
        F: Fn(u64),
    {
        wq.0.notify_one_with(true, func);
    }
}
