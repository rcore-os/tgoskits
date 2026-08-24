//! Linux-style `task_cpu`, `on_rq`, and `on_cpu` publication.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{CpuId, CpuSet, runtime::task_runtime};

const ON_RQ_BITS: u32 = 2;
const CPU_BITS: u32 = 30;
const ON_RQ_MASK: u64 = (1 << ON_RQ_BITS) - 1;
const CPU_MASK: u64 = (1 << CPU_BITS) - 1;
const TASK_CPU_SHIFT: u32 = ON_RQ_BITS;

const ON_RQ_NONE: u64 = 0;
const ON_RQ_QUEUED: u64 = 1;
const ON_RQ_MIGRATING: u64 = 2;

#[cfg(feature = "task-test-hooks")]
static ON_CPU_WAITS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "task-test-hooks")]
static ON_CPU_WAIT_ITERATIONS: AtomicU64 = AtomicU64::new(0);

/// Linux-compatible runqueue ownership state.
///
/// `Queued` is `TASK_ON_RQ_QUEUED`; it remains set while RT/Deadline current
/// stays linked in its class structure. `Migrating` is the only rq-to-rq
/// carrier state. Switch-out and exit are deliberately absent: those belong
/// exclusively to the owner CPU's move-only switch handoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskOnRunQueue {
    None,
    Queued,
    Migrating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlacementSnapshot {
    task_cpu: Option<CpuId>,
    on_rq: TaskOnRunQueue,
}

/// Publication of Linux's orthogonal task placement facts.
///
/// The owning rq transaction changes the packed `task_cpu`/`on_rq` pair.
/// `on_cpu` is a separate execution claim: selection publishes it with one
/// store and switch tail clears it with a Release store, matching Linux's
/// `prepare_task()`/`finish_task()` contract. Readers that need a compound
/// placement decision use the task scheduler lock and the owning rq lock;
/// `on_cpu` is not a transaction version for those other facts.
#[derive(Debug)]
pub(in crate::system) struct SchedulerPlacement {
    state: AtomicU64,
    on_cpu: AtomicU64,
    requested_cpu: AtomicU64,
    #[cfg(any(test, all(axtest, feature = "axtest")))]
    on_cpu_rmw_publications: AtomicU64,
    #[cfg(any(test, all(axtest, feature = "axtest")))]
    on_cpu_store_publications: AtomicU64,
}

impl SchedulerPlacement {
    /// Initializes Linux's `task_cpu()` before the task is published.
    ///
    /// This mirrors `sched_cgroup_fork()`/`__set_task_cpu()`: a new task is
    /// not runnable yet, but PI and policy transactions already have one rq
    /// whose lock serializes its scheduler state.
    pub(super) const fn new(task_cpu: CpuId) -> Self {
        Self {
            state: AtomicU64::new(encode(PlacementSnapshot {
                task_cpu: Some(task_cpu),
                on_rq: TaskOnRunQueue::None,
            })),
            on_cpu: AtomicU64::new(0),
            requested_cpu: AtomicU64::new(0),
            #[cfg(any(test, all(axtest, feature = "axtest")))]
            on_cpu_rmw_publications: AtomicU64::new(0),
            #[cfg(any(test, all(axtest, feature = "axtest")))]
            on_cpu_store_publications: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> PlacementSnapshot {
        decode(self.state.load(Ordering::Acquire))
    }

    pub(in crate::system) fn queued_cpu(&self) -> Option<CpuId> {
        let state = self.snapshot();
        (state.on_rq == TaskOnRunQueue::Queued)
            .then_some(state.task_cpu)
            .flatten()
    }

    pub(in crate::system) fn on_cpu(&self) -> Option<CpuId> {
        decode_cpu(self.on_cpu.load(Ordering::Acquire))
    }

    /// Waits until switch tail releases Linux's `p->on_cpu` execution claim.
    ///
    /// The waker holds the task scheduler lock while waiting, matching
    /// `try_to_wake_up()` under `p->pi_lock`. The acquire load pairs with
    /// `finish_task()` so runnable activation and enqueue cannot overtake the
    /// previous stack's final scheduler publications.
    pub(in crate::system) fn wait_until_not_on_cpu(&self) {
        #[cfg(feature = "task-test-hooks")]
        let mut wait_iterations = 0u64;
        #[cfg(feature = "task-test-hooks")]
        let mut wait_recorded = false;
        while self.on_cpu().is_some() {
            #[cfg(feature = "task-test-hooks")]
            {
                if !wait_recorded {
                    ON_CPU_WAITS.fetch_add(1, Ordering::Relaxed);
                    wait_recorded = true;
                }
                wait_iterations = wait_iterations.saturating_add(1);
            }
            core::hint::spin_loop();
        }
        #[cfg(feature = "task-test-hooks")]
        if wait_iterations != 0 {
            ON_CPU_WAIT_ITERATIONS.fetch_add(wait_iterations, Ordering::Relaxed);
        }
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(in crate::system) fn task_cpu(&self) -> Option<CpuId> {
        self.snapshot().task_cpu
    }

    pub(in crate::system) fn committed_migration_target(&self) -> Option<CpuId> {
        let state = self.snapshot();
        (state.on_rq == TaskOnRunQueue::Migrating)
            .then_some(state.task_cpu)
            .flatten()
    }

    pub(in crate::system) fn has_pending_migration(&self) -> bool {
        self.committed_migration_target().is_some() || self.requested_migration().is_some()
    }

    /// Linux `task_cpu()`: the last committed rq assignment.
    pub(in crate::system) fn assigned_cpu(&self) -> Option<CpuId> {
        self.snapshot().task_cpu
    }

    /// Returns the CPU that may mutate this task's physical rq/on_cpu state.
    ///
    /// A switching migration is still controlled by the source CPU until
    /// switch tail releases `on_cpu`; otherwise the rq named by `task_cpu`
    /// owns queued or migrating state. A sleeping task has neither physical
    /// owner even though Linux retains its last `task_cpu` as a wake hint.
    pub(in crate::system) fn control_owner(&self) -> Option<CpuId> {
        if let Some(owner) = self.on_cpu() {
            return Some(owner);
        }
        let state = self.snapshot();
        match state.on_rq {
            TaskOnRunQueue::Queued | TaskOnRunQueue::Migrating => state.task_cpu,
            TaskOnRunQueue::None => None,
        }
    }

    /// Linux `activate_task()`.
    pub(in crate::system) fn activate(&self, cpu: CpuId) {
        placement_invariant(self.on_cpu().is_none(), 0x504c_0001, cpu.as_u32() as usize);
        self.transition(0x504c_0001, cpu.as_u32() as usize, |state| {
            let valid = match state.on_rq {
                TaskOnRunQueue::None => true,
                TaskOnRunQueue::Migrating => state.task_cpu == Some(cpu),
                TaskOnRunQueue::Queued => false,
            };
            valid.then_some(PlacementSnapshot {
                task_cpu: Some(cpu),
                on_rq: TaskOnRunQueue::Queued,
            })
        });
        self.clear_requested_cpu(cpu);
    }

    /// Linux `init_idle()`: pins the per-CPU idle task to its rq without
    /// linking it into any scheduling-class queue or incrementing
    /// `rq->nr_running`.
    pub(in crate::system) fn install_idle(&self, cpu: CpuId) {
        placement_invariant(self.on_cpu().is_none(), 0x504c_000d, cpu.as_u32() as usize);
        self.transition(0x504c_000d, cpu.as_u32() as usize, |state| {
            (state.task_cpu == Some(cpu) && state.on_rq == TaskOnRunQueue::None).then_some(
                PlacementSnapshot {
                    task_cpu: Some(cpu),
                    on_rq: TaskOnRunQueue::Queued,
                },
            )
        });
        self.clear_requested_cpu(cpu);
    }

    /// Removes a non-running task from its rq.
    pub(in crate::system) fn deactivate(&self, cpu: CpuId) {
        placement_invariant(self.on_cpu().is_none(), 0x504c_0002, cpu.as_u32() as usize);
        self.transition(0x504c_0002, cpu.as_u32() as usize, |state| {
            (state.on_rq == TaskOnRunQueue::Queued && state.task_cpu == Some(cpu)).then_some(
                PlacementSnapshot {
                    task_cpu: Some(cpu),
                    on_rq: TaskOnRunQueue::None,
                },
            )
        });
        self.requested_cpu.store(0, Ordering::Release);
    }

    /// Reserves the immutable destination of an off-rq wake publication.
    pub(in crate::system) fn begin_remote_wakeup(&self, target: CpuId) {
        placement_invariant(
            self.on_cpu().is_none(),
            0x504c_0003,
            target.as_u32() as usize,
        );
        self.transition(0x504c_0003, target.as_u32() as usize, |state| {
            (state.on_rq == TaskOnRunQueue::None).then_some(PlacementSnapshot {
                task_cpu: Some(target),
                on_rq: TaskOnRunQueue::Migrating,
            })
        });
        self.requested_cpu.store(0, Ordering::Release);
    }

    /// Records an affinity request without retargeting a committed carrier.
    pub(in crate::system) fn request_migration(&self, target: Option<CpuId>) {
        let state = self.snapshot();
        let requested = match state.on_rq {
            TaskOnRunQueue::Queued | TaskOnRunQueue::Migrating => {
                target.filter(|target| Some(*target) != state.task_cpu)
            }
            TaskOnRunQueue::None => {
                placement_invariant(
                    self.on_cpu().is_none(),
                    0x504c_0004,
                    target.map_or(usize::MAX, |cpu| cpu.as_u32() as usize),
                );
                None
            }
        };
        self.requested_cpu
            .store(encode_cpu(requested), Ordering::Release);
    }

    pub(in crate::system) fn requested_migration(&self) -> Option<CpuId> {
        decode_cpu(self.requested_cpu.load(Ordering::Acquire))
    }

    /// Linux `put_prev_task()`: rq and execution ownership remain intact.
    pub(in crate::system) fn put_prev(&self, cpu: CpuId) {
        let state = self.snapshot();
        placement_invariant(
            state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(cpu)
                && self.on_cpu() == Some(cpu)
                && self.requested_migration().is_none(),
            0x504c_0005,
            cpu.as_u32() as usize,
        );
    }

    /// Commits `TASK_ON_RQ_MIGRATING` and the destination `task_cpu()`.
    pub(in crate::system) fn begin_migration(&self, source: CpuId, target: CpuId) {
        placement_invariant(
            self.on_cpu().is_none_or(|owner| owner == source),
            0x504c_0006,
            source.as_u32() as usize,
        );
        self.transition(0x504c_0006, source.as_u32() as usize, |state| {
            (source != target
                && state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(source))
            .then_some(PlacementSnapshot {
                task_cpu: Some(target),
                on_rq: TaskOnRunQueue::Migrating,
            })
        });
        self.requested_cpu.store(0, Ordering::Release);
    }

    /// Removes current from rq while switch tail retains `on_cpu`.
    pub(in crate::system) fn block_current(&self, cpu: CpuId) {
        placement_invariant(
            self.on_cpu() == Some(cpu),
            0x504c_0007,
            cpu.as_u32() as usize,
        );
        self.transition(0x504c_0007, cpu.as_u32() as usize, |state| {
            (state.on_rq == TaskOnRunQueue::Queued && state.task_cpu == Some(cpu)).then_some(
                PlacementSnapshot {
                    task_cpu: Some(cpu),
                    on_rq: TaskOnRunQueue::None,
                },
            )
        });
        self.requested_cpu.store(0, Ordering::Release);
    }

    /// Retains `TASK_ON_RQ_QUEUED` for a Fair task delayed on sleep.
    ///
    /// Linux leaves both `on_rq` and `on_cpu` set until the architecture
    /// switch tail, while `sched_delayed` makes the task non-runnable. The rq
    /// node owns that extra state; this method validates the packed placement
    /// tuple without manufacturing another carrier state.
    pub(in crate::system) fn delay_dequeue_current(&self, cpu: CpuId) {
        let state = self.snapshot();
        placement_invariant(
            state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(cpu)
                && self.on_cpu() == Some(cpu),
            0x504c_000e,
            cpu.as_u32() as usize,
        );
        self.requested_cpu.store(0, Ordering::Release);
    }

    /// Completes Linux `__block_task()` for a delayed Fair entity.
    ///
    /// The entity can still carry the outgoing `on_cpu` claim until switch
    /// tail. Only rq membership is cleared here.
    pub(in crate::system) fn finish_delayed_dequeue(&self, cpu: CpuId) {
        placement_invariant(
            self.on_cpu().is_none_or(|owner| owner == cpu),
            0x504c_000f,
            cpu.as_u32() as usize,
        );
        self.transition(0x504c_000f, cpu.as_u32() as usize, |state| {
            (state.on_rq == TaskOnRunQueue::Queued && state.task_cpu == Some(cpu)).then_some(
                PlacementSnapshot {
                    on_rq: TaskOnRunQueue::None,
                    ..state
                },
            )
        });
        self.requested_cpu.store(0, Ordering::Release);
    }

    /// Linux `set_next_task()`.
    pub(in crate::system) fn set_next_task(&self, cpu: CpuId) {
        self.publish_on_cpu(cpu, 0x504c_0008);
    }

    /// Linux idle-class `set_next_task_idle()`: idle remains logically on its
    /// rq but is never represented in a scheduling-class queue.
    pub(in crate::system) fn set_next_idle(&self, cpu: CpuId) {
        self.publish_on_cpu(cpu, 0x504c_000b);
    }

    /// Linux idle-class `put_prev_task_idle()` retains logical rq membership
    /// and the physical `on_cpu` claim until switch tail.
    pub(in crate::system) fn put_prev_idle(&self, cpu: CpuId) {
        let state = self.snapshot();
        placement_invariant(
            state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(cpu)
                && self.on_cpu() == Some(cpu),
            0x504c_000c,
            cpu.as_u32() as usize,
        );
    }

    /// Linux `finish_task()`: the switch tail release of `on_cpu`.
    pub(in crate::system) fn finish_task(&self, cpu: CpuId) {
        placement_invariant(
            self.on_cpu() == Some(cpu),
            0x504c_0009,
            cpu.as_u32() as usize,
        );
        self.on_cpu.store(0, Ordering::Release);
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        self.on_cpu_store_publications
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Cancels only an unconsumed off-rq carrier during task exit.
    pub(in crate::system) fn cancel_remote_handoff_for_exit(&self) {
        let state = self.snapshot();
        placement_invariant(
            self.on_cpu().is_none() && state.on_rq != TaskOnRunQueue::Queued,
            0x504c_000a,
            state
                .task_cpu
                .map_or(usize::MAX, |cpu| cpu.as_u32() as usize),
        );
        self.transition(0x504c_000a, 0, |current| {
            (current == state).then_some(PlacementSnapshot {
                on_rq: TaskOnRunQueue::None,
                ..current
            })
        });
        self.requested_cpu.store(0, Ordering::Release);
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(in crate::system) fn inject_missing_on_cpu(&self) {
        self.on_cpu.store(0, Ordering::Release);
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(in crate::system) fn inject_exiting_on_cpu(&self, cpu: CpuId) {
        self.state.store(
            encode(PlacementSnapshot {
                task_cpu: Some(cpu),
                on_rq: TaskOnRunQueue::None,
            }),
            Ordering::Release,
        );
        self.on_cpu.store(encode_cpu(Some(cpu)), Ordering::Release);
        self.requested_cpu.store(0, Ordering::Release);
    }

    fn publish_on_cpu(&self, cpu: CpuId, code: u32) {
        let state = self.snapshot();
        placement_invariant(
            state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(cpu)
                && self.on_cpu().is_none_or(|owner| owner == cpu),
            code,
            cpu.as_u32() as usize,
        );
        self.on_cpu.store(encode_cpu(Some(cpu)), Ordering::Release);
        #[cfg(any(test, all(axtest, feature = "axtest")))]
        self.on_cpu_store_publications
            .fetch_add(1, Ordering::Relaxed);
    }

    fn clear_requested_cpu(&self, committed: CpuId) {
        let encoded = encode_cpu(Some(committed));
        let _ =
            self.requested_cpu
                .compare_exchange(encoded, 0, Ordering::AcqRel, Ordering::Acquire);
    }

    fn transition(
        &self,
        code: u32,
        detail: usize,
        mut operation: impl FnMut(PlacementSnapshot) -> Option<PlacementSnapshot>,
    ) {
        let mut encoded = self.state.load(Ordering::Acquire);
        loop {
            let current = decode(encoded);
            let Some(next) = operation(current) else {
                task_runtime::fatal_invariant(code, detail);
            };
            let next = encode(next);
            if next == encoded {
                return;
            }
            match self.state.compare_exchange_weak(
                encoded,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => encoded = actual,
            }
        }
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    fn take_on_cpu_publications(&self) -> (u64, u64) {
        (
            self.on_cpu_rmw_publications.swap(0, Ordering::AcqRel),
            self.on_cpu_store_publications.swap(0, Ordering::AcqRel),
        )
    }
}

#[cfg(all(axtest, feature = "axtest"))]
pub(crate) fn exercise_on_cpu_publication_kinds() -> (u64, u64, u64, u64) {
    let cpu = CpuId::new(0);
    let placement = SchedulerPlacement::new(cpu);
    placement.activate(cpu);
    let _ = placement.take_on_cpu_publications();

    placement.set_next_task(cpu);
    let (set_next_rmw, set_next_store) = placement.take_on_cpu_publications();
    placement.finish_task(cpu);
    let (finish_rmw, finish_store) = placement.take_on_cpu_publications();

    (set_next_rmw, set_next_store, finish_rmw, finish_store)
}

#[cfg(feature = "task-test-hooks")]
pub(crate) fn take_on_cpu_waits() -> (u64, u64) {
    (
        ON_CPU_WAITS.swap(0, Ordering::AcqRel),
        ON_CPU_WAIT_ITERATIONS.swap(0, Ordering::AcqRel),
    )
}

/// Affinity policy remains under the task control lock.
#[derive(Debug)]
pub(in crate::system) struct ThreadAffinityState {
    pub(in crate::system) affinity: Arc<CpuSet>,
    pub(in crate::system) affinity_generation: u64,
}

impl ThreadAffinityState {
    pub(super) fn new(affinity: CpuSet) -> Self {
        Self {
            affinity: Arc::new(affinity),
            affinity_generation: 1,
        }
    }
}

const fn encode_cpu(cpu: Option<CpuId>) -> u64 {
    match cpu {
        Some(cpu) => cpu.as_u32() as u64 + 1,
        None => 0,
    }
}

const fn decode_cpu(encoded: u64) -> Option<CpuId> {
    if encoded == 0 {
        None
    } else {
        Some(CpuId::new((encoded - 1) as u32))
    }
}

const fn encode(state: PlacementSnapshot) -> u64 {
    let on_rq = match state.on_rq {
        TaskOnRunQueue::None => ON_RQ_NONE,
        TaskOnRunQueue::Queued => ON_RQ_QUEUED,
        TaskOnRunQueue::Migrating => ON_RQ_MIGRATING,
    };
    on_rq | ((encode_cpu(state.task_cpu) & CPU_MASK) << TASK_CPU_SHIFT)
}

fn decode(encoded: u64) -> PlacementSnapshot {
    let on_rq = match encoded & ON_RQ_MASK {
        ON_RQ_NONE => TaskOnRunQueue::None,
        ON_RQ_QUEUED => TaskOnRunQueue::Queued,
        ON_RQ_MIGRATING => TaskOnRunQueue::Migrating,
        _ => task_runtime::fatal_invariant(0x504c_00fe, encoded as usize),
    };
    PlacementSnapshot {
        task_cpu: decode_cpu((encoded >> TASK_CPU_SHIFT) & CPU_MASK),
        on_rq,
    }
}

fn placement_invariant(valid: bool, code: u32, detail: usize) {
    if !valid {
        task_runtime::fatal_invariant(code, detail);
    }
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use super::*;

    const CPU0: CpuId = CpuId::new(0);
    const CPU1: CpuId = CpuId::new(1);

    fn running_placement() -> SchedulerPlacement {
        let placement = SchedulerPlacement::new(CPU0);
        placement.activate(CPU0);
        placement.set_next_task(CPU0);
        placement
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn current_remains_on_rq_across_put_prev() {
        let placement = running_placement();
        placement.put_prev(CPU0);
        assert_eq!(placement.queued_cpu(), Some(CPU0));
        assert_eq!(placement.on_cpu(), Some(CPU0));

        placement.finish_task(CPU0);
        assert_eq!(placement.queued_cpu(), Some(CPU0));
        assert_eq!(placement.on_cpu(), None);
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn migration_and_execution_release_remain_orthogonal() {
        let placement = running_placement();
        placement.begin_migration(CPU0, CPU1);

        assert_eq!(placement.queued_cpu(), None);
        assert_eq!(placement.task_cpu(), Some(CPU1));
        assert_eq!(placement.committed_migration_target(), Some(CPU1));
        assert_eq!(placement.on_cpu(), Some(CPU0));

        placement.finish_task(CPU0);
        assert_eq!(placement.committed_migration_target(), Some(CPU1));
        assert_eq!(placement.on_cpu(), None);
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn blocked_task_retains_task_cpu_after_switch_tail() {
        let placement = running_placement();
        placement.block_current(CPU0);
        placement.finish_task(CPU0);

        assert_eq!(placement.assigned_cpu(), Some(CPU0));
        assert_eq!(placement.queued_cpu(), None);
        assert_eq!(placement.on_cpu(), None);
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn committed_carrier_is_not_retargeted_by_a_later_request() {
        let placement = SchedulerPlacement::new(CPU0);
        placement.begin_remote_wakeup(CPU1);
        placement.request_migration(Some(CPU0));

        assert_eq!(placement.committed_migration_target(), Some(CPU1));
        assert_eq!(placement.requested_migration(), Some(CPU0));
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn on_cpu_switch_publications_are_linux_style_stores() {
        let placement = SchedulerPlacement::new(CPU0);
        placement.activate(CPU0);
        assert_eq!(placement.take_on_cpu_publications(), (0, 0));

        placement.set_next_task(CPU0);
        assert_eq!(placement.take_on_cpu_publications(), (0, 1));

        placement.finish_task(CPU0);
        assert_eq!(placement.take_on_cpu_publications(), (0, 1));
    }
}
