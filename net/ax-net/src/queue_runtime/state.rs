use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use rd_net::NetPollGroupId;

use super::{
    QueueNotification, STATE_DISABLED, STATE_IDLE, STATE_MASK, STATE_MISSED, STATE_POLLING,
    STATE_SCHEDULED,
};
use crate::observe::{QueueRearmOutcome, QueueRearmReport, report_queue_rearm};

/// Immutable identity of one poll group.
///
/// The fields are fixed while the runtime is built and never change afterwards,
/// so a snapshot taken at any time can be attributed to the same queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetQueueIdentity {
    /// Index of the device in the order the runtime was built with, i.e. its
    /// discovery order.  Startup can skip a device, so this index is not the
    /// published interface order that `InterfaceId` and the interface names
    /// follow; the runtime keeps the two apart.
    pub discovery_order: usize,
    /// Queue group id assigned by the device driver.  It is local to its
    /// device, so `(discovery_order, group_id)` is the key that identifies a
    /// poll group.
    pub group_id: NetPollGroupId,
    /// CPU that owns the group's hard IRQ callback and queue executor.
    pub owner_cpu: usize,
}

/// Counters observed for one poll group, captured as a copyable snapshot.
///
/// Every field is read from its own atomic, so the values are independent
/// samples rather than one instant of the group: a single field is monotonic
/// and current, but combinations of fields may not describe the same moment.
/// Consumers must not derive cross-field invariants (for example that
/// `irq_to_poll_remote_wake` never exceeds `irq`) from one snapshot.
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
    pub last_irq_cpu: Option<usize>,
    pub last_poll_cpu: Option<usize>,
    pub irq_to_poll_remote_wake: u64,
    /// Frames dropped on this group's RX path; never reset.
    ///
    /// The device layer folds the same drops, through a separate increment,
    /// into the interface `rx_dropped` counter; the two count the same events
    /// and must not be added together.
    pub rx_drops: u64,
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
    pub(super) rx_drops: AtomicU64,
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
            rx_drops: AtomicU64::new(0),
        }
    }

    pub(super) fn snapshot(&self) -> NetQueueStats {
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
            last_irq_cpu: optional_cpu(self.last_irq_cpu.load(Ordering::Acquire)),
            last_poll_cpu: optional_cpu(self.last_poll_cpu.load(Ordering::Acquire)),
            irq_to_poll_remote_wake: self.irq_to_poll_remote_wake.load(Ordering::Relaxed),
            rx_drops: self.rx_drops.load(Ordering::Relaxed),
        }
    }
}

/// Shared atomic state for one poll group.
pub(super) struct PollGroupState {
    pub(super) identity: NetQueueIdentity,
    pub(super) state: AtomicU8,
    startup_absent: AtomicBool,
    notify: Arc<QueueNotification>,
    pub(super) stats: QueueStatsAtomic,
    /// RX drops not yet folded into the interface counter.  The device layer
    /// drains this increment while `stats.rx_drops` keeps the cumulative value.
    rx_drops_pending: AtomicU64,
}

impl PollGroupState {
    pub(super) fn new(identity: NetQueueIdentity, notify: Arc<QueueNotification>) -> Self {
        Self {
            state: AtomicU8::new(STATE_DISABLED),
            startup_absent: AtomicBool::new(false),
            identity,
            notify,
            stats: QueueStatsAtomic::new(),
            rx_drops_pending: AtomicU64::new(0),
        }
    }

    pub(super) fn mark_startup_absent(&self) {
        // The owner publishes this only after startup cancellation or shutdown has proved
        // that the unpublished group can be released. The builder's acquire
        // load precedes IRQ synchronization and removal of protocol endpoints.
        self.startup_absent.store(true, Ordering::Release);
    }

    pub(super) fn startup_absent(&self) -> bool {
        self.startup_absent.load(Ordering::Acquire)
    }

    pub(super) fn record_rx_drop(&self) {
        // Statistics only; packet ownership is published through the queues.
        // The cumulative value stays with the group while the pending
        // increment is what the device layer folds into the interface counter.
        self.stats.rx_drops.fetch_add(1, Ordering::Relaxed);
        self.rx_drops_pending.fetch_add(1, Ordering::Relaxed);
    }

    /// Takes the RX drops observed since the previous call.  The cumulative
    /// value reported by `stats.rx_drops` is left untouched.
    pub(super) fn take_pending_rx_drops(&self) -> u64 {
        self.rx_drops_pending.swap(0, Ordering::Relaxed)
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
        if self.startup_absent() {
            return;
        }
        if cpu != self.identity.owner_cpu {
            self.stats
                .irq_to_poll_remote_wake
                .fetch_add(1, Ordering::Relaxed);
            self.disable();
            return;
        }
        if self.is_disabled() {
            // During owner startup queues stay disabled, but the startup
            // state machine still needs the IRQ notification to advance.
            self.notify.notify();
        } else if self.publish_schedule() {
            self.notify.notify();
        }
    }

    pub(super) fn wait_startup_irq(&self, waiter: &ax_task::sync::irq::IrqWorkerWaiter) {
        self.notify.wait(waiter);
    }

    pub(super) fn wait_startup_deadline(
        &self,
        waiter: &ax_task::sync::irq::IrqWorkerWaiter,
        deadline_nanos: u64,
    ) {
        let now = ax_hal::time::monotonic_time_nanos();
        if deadline_nanos > now {
            let duration = core::time::Duration::from_nanos(deadline_nanos - now);
            self.notify.wait_timeout(waiter, duration);
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
        if current_cpu != self.identity.owner_cpu {
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
                    // An IRQ arrived while the round was polling: the group
                    // goes straight back to scheduled and the hardware rearm
                    // is skipped.  This is the only place the race outcome is
                    // observable, so the event is reported from the
                    // transition itself.
                    report_queue_rearm(QueueRearmReport {
                        identity: self.identity,
                        outcome: QueueRearmOutcome::Race,
                    });
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
