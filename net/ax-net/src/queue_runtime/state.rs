use alloc::sync::Arc;
use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use super::{STATE_DISABLED, STATE_IDLE, STATE_MASK, STATE_MISSED, STATE_POLLING, STATE_SCHEDULED};

/// Observable queue statistics used by SMP contract tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetQueueStats {
    pub irq: u64,
    pub schedule: u64,
    pub missed: u64,
    pub poll_batches: u64,
    pub budget_exhaustion: u64,
    pub spurious: u64,
    pub probe_deferred: u64,
    pub rearm_race: u64,
    pub owner_cpu: usize,
    pub last_irq_cpu: Option<usize>,
    pub last_poll_cpu: Option<usize>,
    pub irq_to_poll_remote_wake: u64,
}

pub(super) struct QueueStatsAtomic {
    pub(super) irq: AtomicU64,
    pub(super) schedule: AtomicU64,
    pub(super) missed: AtomicU64,
    pub(super) poll_batches: AtomicU64,
    pub(super) budget_exhaustion: AtomicU64,
    pub(super) spurious: AtomicU64,
    pub(super) probe_deferred: AtomicU64,
    pub(super) rearm_race: AtomicU64,
    pub(super) last_irq_cpu: AtomicUsize,
    pub(super) last_poll_cpu: AtomicUsize,
    pub(super) irq_to_poll_remote_wake: AtomicU64,
}

impl QueueStatsAtomic {
    const fn new() -> Self {
        Self {
            irq: AtomicU64::new(0),
            schedule: AtomicU64::new(0),
            missed: AtomicU64::new(0),
            poll_batches: AtomicU64::new(0),
            budget_exhaustion: AtomicU64::new(0),
            spurious: AtomicU64::new(0),
            probe_deferred: AtomicU64::new(0),
            rearm_race: AtomicU64::new(0),
            last_irq_cpu: AtomicUsize::new(usize::MAX),
            last_poll_cpu: AtomicUsize::new(usize::MAX),
            irq_to_poll_remote_wake: AtomicU64::new(0),
        }
    }

    pub(super) fn snapshot(&self, owner_cpu: usize) -> NetQueueStats {
        let optional_cpu = |cpu| (cpu != usize::MAX).then_some(cpu);
        NetQueueStats {
            irq: self.irq.load(Ordering::Relaxed),
            schedule: self.schedule.load(Ordering::Relaxed),
            missed: self.missed.load(Ordering::Relaxed),
            poll_batches: self.poll_batches.load(Ordering::Relaxed),
            budget_exhaustion: self.budget_exhaustion.load(Ordering::Relaxed),
            spurious: self.spurious.load(Ordering::Relaxed),
            probe_deferred: self.probe_deferred.load(Ordering::Relaxed),
            rearm_race: self.rearm_race.load(Ordering::Relaxed),
            owner_cpu,
            last_irq_cpu: optional_cpu(self.last_irq_cpu.load(Ordering::Acquire)),
            last_poll_cpu: optional_cpu(self.last_poll_cpu.load(Ordering::Acquire)),
            irq_to_poll_remote_wake: self.irq_to_poll_remote_wake.load(Ordering::Relaxed),
        }
    }
}

/// Shared atomic state for one poll group.
pub(super) struct PollGroupState {
    pub(super) state: AtomicU8,
    pub(super) owner_cpu: usize,
    notify: Arc<ax_task::IrqNotify>,
    pub(super) stats: QueueStatsAtomic,
    rx_drops: AtomicU64,
}

impl PollGroupState {
    pub(super) fn new(owner_cpu: usize, notify: Arc<ax_task::IrqNotify>) -> Self {
        Self {
            state: AtomicU8::new(STATE_DISABLED),
            owner_cpu,
            notify,
            stats: QueueStatsAtomic::new(),
            rx_drops: AtomicU64::new(0),
        }
    }

    pub(super) fn record_rx_drop(&self) {
        // Statistics only; packet ownership is published through the queues.
        self.rx_drops.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn take_rx_drops(&self) -> u64 {
        self.rx_drops.swap(0, Ordering::Relaxed)
    }

    pub(super) fn activate(&self, pending: bool) {
        self.state.store(STATE_IDLE, Ordering::Release);
        if pending {
            self.schedule_task();
        }
    }

    pub(super) fn schedule_irq(&self) {
        let cpu = ax_hal::percpu::this_cpu_id();
        self.stats.irq.fetch_add(1, Ordering::Relaxed);
        self.stats.last_irq_cpu.store(cpu, Ordering::Release);
        if cpu != self.owner_cpu {
            self.stats
                .irq_to_poll_remote_wake
                .fetch_add(1, Ordering::Relaxed);
            self.disable();
            return;
        }
        if self.is_disabled() {
            // During owner startup queues stay disabled, but the startup
            // state machine still needs the IRQ notification to advance.
            self.notify.notify_irq();
        } else if self.publish_schedule() {
            self.notify.notify_irq();
        }
    }

    pub(super) fn wait_startup_irq(&self) {
        self.notify.wait();
    }

    pub(super) fn wait_startup_deadline(&self, deadline_nanos: u64) {
        let now = ax_hal::time::monotonic_time_nanos();
        if deadline_nanos > now {
            let duration = core::time::Duration::from_nanos(deadline_nanos - now);
            self.notify.wait_timeout(duration);
        }
    }

    pub(super) fn schedule_task(&self) {
        self.publish_schedule();
        // A task-side publication can be what releases a queue executor that
        // stopped on RX/TX ring backpressure. In that case the state is
        // POLLING|MISSED rather than a fresh IDLE->SCHEDULED transition, but
        // the sleeping owner still needs a precise wakeup.
        if !self.is_disabled() {
            self.notify.notify();
        }
    }

    fn publish_schedule(&self) -> bool {
        loop {
            let old = self.state.load(Ordering::Acquire);
            match old & STATE_MASK {
                STATE_DISABLED => return false,
                STATE_IDLE => {
                    if self
                        .state
                        .compare_exchange(old, STATE_SCHEDULED, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        self.stats.schedule.fetch_add(1, Ordering::Relaxed);
                        return true;
                    }
                }
                STATE_SCHEDULED | STATE_POLLING => {
                    if old & STATE_MISSED != 0 {
                        return false;
                    }
                    if self
                        .state
                        .compare_exchange(
                            old,
                            old | STATE_MISSED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.stats.missed.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }

    pub(super) fn claim(&self) -> bool {
        let current_cpu = ax_hal::percpu::this_cpu_id();
        if current_cpu != self.owner_cpu {
            self.disable();
            return false;
        }
        loop {
            let old = self.state.load(Ordering::Acquire);
            let claimable = (old & STATE_MASK == STATE_SCHEDULED)
                || (old & STATE_MASK == STATE_POLLING && old & STATE_MISSED != 0);
            if !claimable {
                return false;
            }
            if self
                .state
                .compare_exchange(old, STATE_POLLING, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.stats
                    .last_poll_cpu
                    .store(current_cpu, Ordering::Release);
                self.stats.poll_batches.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
    }

    pub(super) fn finish_more(&self) {
        loop {
            let old = self.state.load(Ordering::Acquire);
            if old & STATE_MASK != STATE_POLLING {
                return;
            }
            if self
                .state
                .compare_exchange(old, STATE_SCHEDULED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    pub(super) fn begin_rearm(&self) -> bool {
        loop {
            let old = self.state.load(Ordering::Acquire);
            if old & STATE_MASK != STATE_POLLING {
                return false;
            }
            if old & STATE_MISSED != 0 {
                if self
                    .state
                    .compare_exchange(old, STATE_SCHEDULED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return false;
                }
                continue;
            }
            if self
                .state
                .compare_exchange(old, STATE_IDLE, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    pub(super) fn disable(&self) {
        self.state.store(STATE_DISABLED, Ordering::Release);
        self.notify.notify();
    }

    pub(super) fn is_disabled(&self) -> bool {
        self.state.load(Ordering::Acquire) & STATE_MASK == STATE_DISABLED
    }
}
