//! Wake publication, park generation, and task-deadline binding.

use super::*;

impl ThreadCore {
    pub(crate) fn enter_rt_lock_critical(&self) {
        self.rt_lock_depth
            .try_update(Ordering::AcqRel, Ordering::Acquire, |depth| {
                depth.checked_add(1)
            })
            .expect("RT lock nesting exhausted");
    }

    pub(crate) fn leave_rt_lock_critical(&self) {
        self.rt_lock_depth
            .try_update(Ordering::AcqRel, Ordering::Acquire, |depth| {
                depth.checked_sub(1)
            })
            .expect("RT lock nesting must balance");
    }

    pub(crate) fn holds_rt_lock(&self) -> bool {
        self.rt_lock_depth.load(Ordering::Acquire) != 0
    }

    pub(crate) fn publish_wake(&self) -> WakePublication {
        self.state.publish_wake()
    }

    /// The caller holds the task scheduler lock across saved-state changes.
    pub(crate) fn enter_rt_lock_wait(&self) -> Result<(), TaskError> {
        if self.state.in_rt_lock_wait() {
            return Err(TaskError::InvalidConfiguration);
        }
        self.state.enter_rt_lock_wait()
    }

    /// The task scheduler lock excludes exact-generation notification delivery.
    pub(crate) fn restore_rt_lock_wait(&self) -> Result<(), TaskError> {
        if !self.in_rt_lock_wait() || self.state() != ThreadState::Running {
            return Err(TaskError::InvalidConfiguration);
        }
        self.park_generation.store(
            self.ordinary_park_generation.load(Ordering::Acquire),
            Ordering::Release,
        );
        self.state.restore_rt_lock_wait()
    }

    pub(crate) fn in_rt_lock_wait(&self) -> bool {
        self.state.in_rt_lock_wait()
    }

    pub(crate) fn ordinary_park_generation(&self) -> u64 {
        self.ordinary_park_generation.load(Ordering::Acquire)
    }

    pub(crate) fn publish_rt_lock_wake(&self) -> Option<WakePublication> {
        self.state.publish_rt_lock_wake()
    }

    pub(crate) fn consume_wake_and_transition(
        &self,
        preserve_park_notification: bool,
        next: Option<ThreadState>,
    ) -> (ThreadState, bool) {
        self.state
            .consume_wake_and_transition(preserve_park_notification, next)
    }

    pub(crate) fn discard_failed_wake(&self) {
        self.state.discard_failed_wake();
    }

    pub(crate) fn register_sleep_timer(&self, cpu: CpuId, generation: u64) {
        self.sleep_timer_cpu.store(cpu.as_u32(), Ordering::Relaxed);
        self.sleep_timer_generation
            .store(generation, Ordering::Release);
    }

    pub(crate) fn sleep_timer_cpu(&self) -> Option<CpuId> {
        let generation = self.sleep_timer_generation.load(Ordering::Acquire);
        if generation == 0 {
            return None;
        }
        let cpu = self.sleep_timer_cpu.load(Ordering::Relaxed);
        (cpu != u32::MAX).then(|| CpuId::new(cpu))
    }

    pub(crate) fn sleep_timer_cpu_for(&self, generation: u64) -> Option<CpuId> {
        (self.sleep_timer_generation.load(Ordering::Acquire) == generation)
            .then(|| self.sleep_timer_cpu.load(Ordering::Relaxed))
            .filter(|cpu| *cpu != u32::MAX)
            .map(CpuId::new)
    }

    pub(crate) fn complete_sleep_timer(&self, generation: u64) -> bool {
        if self
            .sleep_timer_generation
            .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.sleep_timer_cpu.store(u32::MAX, Ordering::Release);
        true
    }

    pub(crate) fn take_park_notification(&self) -> bool {
        self.state.take_park_notification()
    }

    pub(crate) fn publish_blocked_from_parking(&self) -> Result<ParkPublication, TaskError> {
        self.state.publish_blocked_from_parking()
    }

    pub(crate) fn next_park_generation(&self) -> Result<u64, TaskError> {
        let mut generation = self.park_sequence.load(Ordering::Acquire);
        loop {
            let next = generation
                .checked_add(1)
                .ok_or(TaskError::InvalidConfiguration)?;
            match self.park_sequence.compare_exchange_weak(
                generation,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.park_generation.store(next, Ordering::Release);
                    if !self.in_rt_lock_wait() {
                        self.ordinary_park_generation.store(next, Ordering::Release);
                    }
                    return Ok(next);
                }
                Err(observed) => generation = observed,
            }
        }
    }

    pub(crate) fn park_generation(&self) -> u64 {
        self.park_generation.load(Ordering::Acquire)
    }

    pub(crate) fn state(&self) -> ThreadState {
        self.state.state()
    }
}
