//! Typed results published across the scheduler/runtime boundary.

use super::super::thread_sched::DeadlineActivity;
use crate::{
    CpuId, SchedulePolicy, SwitchReason, ThreadCore, ThreadExtensionView, ThreadId,
    runtime::{AddressSpaceHandle, ExecutionContextHandle},
};

/// Result of one scheduler safe-point decision.
#[derive(Clone, Copy, Debug)]
pub struct ScheduleDecision {
    pub(super) previous: Option<ThreadId>,
    pub(super) next: ThreadId,
    pub(super) previous_endpoint: Option<SwitchEndpoint>,
    pub(super) next_endpoint: SwitchEndpoint,
    pub(super) switch_reason: SwitchReason,
}

/// Callback work that becomes valid only after the incoming thread is current.
#[doc(hidden)]
pub struct SwitchInCompletion {
    thread: Option<ThreadId>,
    policy: Option<SchedulePolicy>,
    extension: Option<ThreadExtensionView>,
}

impl SwitchInCompletion {
    pub(crate) const NONE: Self = Self {
        thread: None,
        policy: None,
        extension: None,
    };

    pub(crate) fn for_core(core: &ThreadCore) -> Self {
        Self {
            thread: Some(core.id()),
            policy: Some(core.base_policy()),
            extension: core.extension_view(),
        }
    }

    #[doc(hidden)]
    pub fn finish(self) {
        let (Some(thread), Some(policy), Some(extension)) =
            (self.thread, self.policy, self.extension)
        else {
            return;
        };
        // SAFETY: TaskSystem creates this token only after architecture current
        // publication, previous-binding withdrawal, and switch-handoff
        // consumption. The facade drops its CpuLocal owner borrow before
        // finishing the token while retaining the scheduler IRQ baton.
        unsafe { (extension.ops().on_switch_in)(extension.data(), thread, policy) };
    }
}

/// Result of one bounded scheduler safe point.
///
/// This type deliberately keeps lifecycle deferral and bounded owner work
/// separate from a scheduling decision. Callers must not infer either state
/// from a boolean `need_resched` value or an absent decision.
#[derive(Clone, Copy, Debug)]
pub enum SchedulerOutcome {
    /// No context switch or owner-only work remains from this pass.
    Quiescent,
    /// The current thread owns an in-flight park token and must finish it.
    ParkingDeferred,
    /// One bounded owner batch completed, with more work retained.
    OwnerWorkPending,
    /// The scheduler selected a next thread.
    Decision(ScheduleDecision),
}

impl SchedulerOutcome {
    /// Returns the scheduler decision, if this pass selected a thread.
    pub const fn decision(self) -> Option<ScheduleDecision> {
        match self {
            Self::Decision(decision) => Some(decision),
            Self::Quiescent | Self::ParkingDeferred | Self::OwnerWorkPending => None,
        }
    }

    /// Returns whether the caller must finish a pending park handshake before
    /// scheduler task-work callbacks may execute.
    pub const fn parking_deferred(self) -> bool {
        matches!(self, Self::ParkingDeferred)
    }

    /// Returns whether more owner-only work remains for a later bounded safe point.
    pub const fn owner_work_pending(self) -> bool {
        matches!(self, Self::OwnerWorkPending)
    }
}

impl ScheduleDecision {
    /// Returns the thread that stopped running, if any.
    pub const fn previous(self) -> Option<ThreadId> {
        self.previous
    }

    /// Returns the selected thread or CPU idle thread.
    pub const fn next(self) -> ThreadId {
        self.next
    }

    /// Returns why the previous thread relinquished the CPU.
    pub const fn switch_reason(self) -> SwitchReason {
        self.switch_reason
    }

    /// Returns whether the architecture execution context must change.
    pub fn requires_context_switch(self) -> bool {
        self.previous != Some(self.next)
    }

    pub(crate) const fn previous_endpoint(self) -> Option<SwitchEndpoint> {
        self.previous_endpoint
    }

    pub(crate) const fn next_endpoint(self) -> SwitchEndpoint {
        self.next_endpoint
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SwitchEndpoint {
    thread: ThreadId,
    context: ExecutionContextHandle,
    address_space: AddressSpaceHandle,
    extension: Option<ThreadExtensionView>,
}

impl SwitchEndpoint {
    pub(super) fn from_core(core: &ThreadCore) -> Self {
        let sched = core.sched().lock();
        Self {
            thread: core.id(),
            context: sched.runtime.context,
            address_space: sched.runtime.address_space,
            extension: core.extension_view(),
        }
    }

    pub(crate) const fn thread(self) -> ThreadId {
        self.thread
    }

    pub(crate) const fn context(self) -> ExecutionContextHandle {
        self.context
    }

    pub(crate) const fn address_space(self) -> crate::runtime::AddressSpaceHandle {
        self.address_space
    }

    pub(crate) const fn extension(self) -> Option<ThreadExtensionView> {
        self.extension
    }
}

/// Result of charging one scheduler dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChargeOutcome {
    pub(super) slice_expired: bool,
    pub(super) deadline_overrun: bool,
}

/// Snapshot of one Deadline reservation's CBS and PI state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineRuntimeSnapshot {
    pub(super) remaining_runtime_ns: u64,
    pub(super) misses: u64,
    pub(super) overruns: u64,
    pub(super) pi_critical_rescue: bool,
    pub(super) donor: Option<ThreadId>,
}

/// Snapshot of a Deadline thread's GRUB ownership and zero-lag state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineActivitySnapshot {
    pub(super) activity: DeadlineActivity,
    pub(super) bandwidth_cpu: Option<CpuId>,
    pub(super) zero_lag_ns: u64,
}

impl DeadlineActivitySnapshot {
    /// Returns the GRUB state.
    pub const fn activity(self) -> DeadlineActivity {
        self.activity
    }

    /// Returns the runqueue owning this reservation's `this_bw` contribution.
    pub const fn bandwidth_cpu(self) -> Option<CpuId> {
        self.bandwidth_cpu
    }

    /// Returns the pending zero-lag boundary, or zero when no timer is armed.
    pub const fn zero_lag_ns(self) -> u64 {
        self.zero_lag_ns
    }
}

impl DeadlineRuntimeSnapshot {
    /// Returns the remaining CBS runtime.
    pub const fn remaining_runtime_ns(self) -> u64 {
        self.remaining_runtime_ns
    }

    /// Returns observed absolute-deadline misses.
    pub const fn misses(self) -> u64 {
        self.misses
    }

    /// Returns observed CBS overruns.
    pub const fn overruns(self) -> u64 {
        self.overruns
    }

    /// Reports whether execution is in the explicit PI-critical rescue path.
    pub const fn pi_critical_rescue(self) -> bool {
        self.pi_critical_rescue
    }

    /// Returns the original Deadline reservation currently donated to the thread.
    pub const fn donor(self) -> Option<ThreadId> {
        self.donor
    }
}

/// Result of one bounded owner-control drain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerControlDrain {
    pub(super) drained: usize,
    pub(super) pending: bool,
}

impl OwnerControlDrain {
    /// Returns the number of detached control messages consumed.
    pub const fn drained(self) -> usize {
        self.drained
    }

    /// Returns whether another bounded drain is required.
    pub const fn pending(self) -> bool {
        self.pending
    }
}

impl ChargeOutcome {
    /// Returns whether RR, fair service, or CBS budget reached its boundary.
    pub const fn slice_expired(self) -> bool {
        self.slice_expired
    }

    /// Returns whether CBS exhaustion entered a PI-critical rescue section.
    pub const fn deadline_overrun(self) -> bool {
        self.deadline_overrun
    }
}
