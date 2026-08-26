//! CPU-owner scheduler state and switch continuation.

use super::*;
use crate::system::task_system::TaskSystem;
mod dispatch_state;
mod drain_state;
mod owner_deadline;
mod owner_dispatch;
mod owner_idle;

pub(crate) use owner_deadline::{
    HardTimerServiceClaim, KtimerServiceClaim, SchedulerDeadlineRqObservation,
};

use crate::system::cpu::remote::SchedulerDeadlinePublicationState;

/// Outer owner-CPU transition that requested a fresh scheduler deadline derivation.
#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum SchedulerDeadlineDerivationSource {
    ClockEvent,
    ParkArm,
    ParkCancel,
    KernelTimer,
    KtimerService,
    Enqueue,
    Placement,
    ScheduleSelection,
    ScheduleNoSwitch,
}

/// Scheduler state that is created explicitly and mutated only by its owner CPU.
///
/// The object is `!Unpin`; runtimes store it in per-CPU pinned allocations and
/// publish it only after registration has completed.
#[derive(Debug)]
pub struct CpuLocal {
    owner: CpuId,
    remote: Arc<CpuRemote>,
    rt_bandwidth: Arc<RootRtBandwidth>,
    dispatch: dispatch_state::OwnerDispatchState,
    drain: drain_state::OwnerDrainScratch,
    _pinned: PhantomPinned,
}

impl CpuLocal {
    pub(crate) fn create(
        owner: CpuId,
        config: TaskSystemConfig,
        remote: Arc<CpuRemote>,
        rt_bandwidth: Arc<RootRtBandwidth>,
    ) -> Pin<Box<Self>> {
        debug_assert_eq!(owner, remote.owner());
        Box::pin(Self {
            owner,
            remote,
            rt_bandwidth,
            dispatch: dispatch_state::OwnerDispatchState::new(config),
            drain: drain_state::OwnerDrainScratch::new(config),
            _pinned: PhantomPinned,
        })
    }

    /// Returns the logical processor that exclusively owns the run queue.
    pub const fn owner(&self) -> CpuId {
        self.owner
    }

    /// Returns whether registration and online publication have completed.
    pub fn is_online(&self) -> bool {
        self.remote.is_online()
    }

    pub(crate) fn remote(&self) -> &Arc<CpuRemote> {
        &self.remote
    }
}
