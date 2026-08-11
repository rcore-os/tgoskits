//! Linux-style `task_cpu`, `on_rq`, and `on_cpu` publication.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{CpuId, CpuSet, runtime::task_runtime};

const ON_RQ_BITS: u32 = 2;
const CPU_BITS: u32 = 30;
const ON_RQ_MASK: u64 = (1 << ON_RQ_BITS) - 1;
const CPU_MASK: u64 = (1 << CPU_BITS) - 1;
const TASK_CPU_SHIFT: u32 = ON_RQ_BITS;
const ON_CPU_SHIFT: u32 = ON_RQ_BITS + CPU_BITS;

const ON_RQ_NONE: u64 = 0;
const ON_RQ_QUEUED: u64 = 1;
const ON_RQ_MIGRATING: u64 = 2;

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
    on_cpu: Option<CpuId>,
}

/// Atomic publication of Linux's three orthogonal task placement facts.
///
/// The owning rq transaction changes `task_cpu` and `on_rq`; switch tail is
/// the sole writer which releases `on_cpu`. Packing the three facts makes the
/// release race observable as one valid tuple without inventing task-owned
/// `SwitchingOut` or `ExitedAwaitingTail` lifecycle states.
#[derive(Debug)]
pub(in crate::system) struct SchedulerPlacement {
    state: AtomicU64,
    requested_cpu: AtomicU64,
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
                on_cpu: None,
            })),
            requested_cpu: AtomicU64::new(0),
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

    pub(in crate::system) fn execution_cpu(&self) -> Option<CpuId> {
        let state = self.snapshot();
        (state.on_rq == TaskOnRunQueue::Queued && state.task_cpu == state.on_cpu)
            .then_some(state.on_cpu)
            .flatten()
    }

    pub(in crate::system) fn on_cpu(&self) -> Option<CpuId> {
        self.snapshot().on_cpu
    }

    /// Waits until switch tail releases Linux's `p->on_cpu` execution claim.
    ///
    /// The waker holds the task scheduler lock while waiting, matching
    /// `try_to_wake_up()` under `p->pi_lock`. The acquire load pairs with
    /// `finish_task()` so runnable activation and enqueue cannot overtake the
    /// previous stack's final scheduler publications.
    pub(in crate::system) fn wait_until_not_on_cpu(&self) {
        while self.on_cpu().is_some() {
            core::hint::spin_loop();
        }
    }

    #[cfg(test)]
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

    pub(in crate::system) fn can_continue_running_on(&self, cpu: CpuId) -> bool {
        self.execution_cpu() == Some(cpu) && self.requested_migration().is_none()
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
        let state = self.snapshot();
        state.on_cpu.or(match state.on_rq {
            TaskOnRunQueue::Queued | TaskOnRunQueue::Migrating => state.task_cpu,
            TaskOnRunQueue::None => None,
        })
    }

    /// Linux `activate_task()`.
    pub(in crate::system) fn activate(&self, cpu: CpuId) {
        self.transition(0x504c_0001, cpu.as_u32() as usize, |state| {
            let valid = state.on_cpu.is_none()
                && match state.on_rq {
                    TaskOnRunQueue::None => true,
                    TaskOnRunQueue::Migrating => state.task_cpu == Some(cpu),
                    TaskOnRunQueue::Queued => false,
                };
            valid.then_some(PlacementSnapshot {
                task_cpu: Some(cpu),
                on_rq: TaskOnRunQueue::Queued,
                on_cpu: None,
            })
        });
        self.clear_requested_cpu(cpu);
    }

    /// Linux `init_idle()`: pins the per-CPU idle task to its rq without
    /// linking it into any scheduling-class queue or incrementing
    /// `rq->nr_running`.
    pub(in crate::system) fn install_idle(&self, cpu: CpuId) {
        self.transition(0x504c_000d, cpu.as_u32() as usize, |state| {
            (state.task_cpu == Some(cpu)
                && state.on_rq == TaskOnRunQueue::None
                && state.on_cpu.is_none())
            .then_some(PlacementSnapshot {
                task_cpu: Some(cpu),
                on_rq: TaskOnRunQueue::Queued,
                on_cpu: None,
            })
        });
        self.clear_requested_cpu(cpu);
    }

    /// Removes a non-running task from its rq.
    pub(in crate::system) fn deactivate(&self, cpu: CpuId) {
        self.transition(0x504c_0002, cpu.as_u32() as usize, |state| {
            (state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(cpu)
                && state.on_cpu.is_none())
            .then_some(PlacementSnapshot {
                task_cpu: Some(cpu),
                on_rq: TaskOnRunQueue::None,
                on_cpu: None,
            })
        });
        self.requested_cpu.store(0, Ordering::Release);
    }

    /// Reserves the immutable destination of an off-rq wake publication.
    pub(in crate::system) fn begin_remote_wakeup(&self, target: CpuId) {
        self.transition(0x504c_0003, target.as_u32() as usize, |state| {
            (state.on_rq == TaskOnRunQueue::None && state.on_cpu.is_none()).then_some(
                PlacementSnapshot {
                    task_cpu: Some(target),
                    on_rq: TaskOnRunQueue::Migrating,
                    on_cpu: None,
                },
            )
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
                    state.on_cpu.is_none(),
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
                && state.on_cpu == Some(cpu)
                && self.requested_migration().is_none(),
            0x504c_0005,
            cpu.as_u32() as usize,
        );
    }

    /// Commits `TASK_ON_RQ_MIGRATING` and the destination `task_cpu()`.
    pub(in crate::system) fn begin_migration(&self, source: CpuId, target: CpuId) {
        self.transition(0x504c_0006, source.as_u32() as usize, |state| {
            (source != target
                && state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(source)
                && state.on_cpu.is_none_or(|owner| owner == source))
            .then_some(PlacementSnapshot {
                task_cpu: Some(target),
                on_rq: TaskOnRunQueue::Migrating,
                on_cpu: state.on_cpu,
            })
        });
        self.requested_cpu.store(0, Ordering::Release);
    }

    /// Removes current from rq while switch tail retains `on_cpu`.
    pub(in crate::system) fn block_current(&self, cpu: CpuId) {
        self.transition(0x504c_0007, cpu.as_u32() as usize, |state| {
            (state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(cpu)
                && state.on_cpu == Some(cpu))
            .then_some(PlacementSnapshot {
                task_cpu: Some(cpu),
                on_rq: TaskOnRunQueue::None,
                on_cpu: Some(cpu),
            })
        });
        self.requested_cpu.store(0, Ordering::Release);
    }

    /// Linux `set_next_task()`.
    pub(in crate::system) fn set_next_task(&self, cpu: CpuId) {
        self.transition(0x504c_0008, cpu.as_u32() as usize, |state| {
            (state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(cpu)
                && state.on_cpu.is_none_or(|owner| owner == cpu))
            .then_some(PlacementSnapshot {
                on_cpu: Some(cpu),
                ..state
            })
        });
    }

    /// Linux idle-class `set_next_task_idle()`: idle remains logically on its
    /// rq but is never represented in a scheduling-class queue.
    pub(in crate::system) fn set_next_idle(&self, cpu: CpuId) {
        self.transition(0x504c_000b, cpu.as_u32() as usize, |state| {
            (state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(cpu)
                && state.on_cpu.is_none_or(|owner| owner == cpu))
            .then_some(PlacementSnapshot {
                on_cpu: Some(cpu),
                ..state
            })
        });
    }

    /// Linux idle-class `put_prev_task_idle()` retains logical rq membership
    /// and the physical `on_cpu` claim until switch tail.
    pub(in crate::system) fn put_prev_idle(&self, cpu: CpuId) {
        let state = self.snapshot();
        placement_invariant(
            state.on_rq == TaskOnRunQueue::Queued
                && state.task_cpu == Some(cpu)
                && state.on_cpu == Some(cpu),
            0x504c_000c,
            cpu.as_u32() as usize,
        );
    }

    /// Linux `finish_task()`: the switch tail release of `on_cpu`.
    pub(in crate::system) fn finish_task(&self, cpu: CpuId) {
        self.transition(0x504c_0009, cpu.as_u32() as usize, |state| {
            (state.on_cpu == Some(cpu)).then_some(PlacementSnapshot {
                on_cpu: None,
                ..state
            })
        });
    }

    /// Cancels only an unconsumed off-rq carrier during task exit.
    pub(in crate::system) fn cancel_remote_handoff_for_exit(&self) {
        let state = self.snapshot();
        placement_invariant(
            state.on_cpu.is_none() && state.on_rq != TaskOnRunQueue::Queued,
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

    #[cfg(test)]
    pub(in crate::system) fn inject_missing_on_cpu(&self) {
        self.state
            .fetch_and(!(CPU_MASK << ON_CPU_SHIFT), Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(in crate::system) fn inject_exiting_on_cpu(&self, cpu: CpuId) {
        self.state.store(
            encode(PlacementSnapshot {
                task_cpu: Some(cpu),
                on_rq: TaskOnRunQueue::None,
                on_cpu: Some(cpu),
            }),
            Ordering::Release,
        );
        self.requested_cpu.store(0, Ordering::Release);
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
    on_rq
        | ((encode_cpu(state.task_cpu) & CPU_MASK) << TASK_CPU_SHIFT)
        | ((encode_cpu(state.on_cpu) & CPU_MASK) << ON_CPU_SHIFT)
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
        on_cpu: decode_cpu((encoded >> ON_CPU_SHIFT) & CPU_MASK),
    }
}

fn placement_invariant(valid: bool, code: u32, detail: usize) {
    if !valid {
        task_runtime::fatal_invariant(code, detail);
    }
}

#[cfg(test)]
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

    #[test]
    fn current_remains_on_rq_across_put_prev() {
        let placement = running_placement();
        placement.put_prev(CPU0);
        assert_eq!(placement.queued_cpu(), Some(CPU0));
        assert_eq!(placement.on_cpu(), Some(CPU0));

        placement.finish_task(CPU0);
        assert_eq!(placement.queued_cpu(), Some(CPU0));
        assert_eq!(placement.on_cpu(), None);
    }

    #[test]
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

    #[test]
    fn blocked_task_retains_task_cpu_after_switch_tail() {
        let placement = running_placement();
        placement.block_current(CPU0);
        placement.finish_task(CPU0);

        assert_eq!(placement.assigned_cpu(), Some(CPU0));
        assert_eq!(placement.queued_cpu(), None);
        assert_eq!(placement.on_cpu(), None);
    }

    #[test]
    fn committed_carrier_is_not_retargeted_by_a_later_request() {
        let placement = SchedulerPlacement::new(CPU0);
        placement.begin_remote_wakeup(CPU1);
        placement.request_migration(Some(CPU0));

        assert_eq!(placement.committed_migration_target(), Some(CPU1));
        assert_eq!(placement.requested_migration(), Some(CPU0));
    }
}
