//! Pinned owner-CPU scheduler state.

mod clock;
mod dispatch;
mod load;
mod local;
mod remote;
mod snapshot;
mod transaction;

use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    marker::{PhantomData, PhantomPinned},
    ops::Deref,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering},
};

pub(crate) use clock::{RqTaskTime, RunQueueClock, RunQueueClockSnapshot};
#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(crate) use dispatch::CurrentSchedule;
pub(crate) use dispatch::{
    CurrentClassState, CurrentDispatch, CurrentDispatchState, DispatchCharge, DispatchRole,
    SwitchHandoff,
};
use load::SUMMARY_FAIR_PUSHABLE;
pub use load::{CpuLoadSummary, DeadlineBandwidthSnapshot, SchedulingClass};
pub use local::CpuLocal;
pub(crate) use local::{
    HardTimerServiceClaim, KtimerServiceClaim, SchedulerDeadlineDerivationSource,
    SchedulerDeadlineRqObservation,
};
#[cfg(feature = "task-test-hooks")]
pub(crate) use remote::CpuDeadlineState;
use remote::RqCurrentUpdate;
pub use remote::{CpuLifecycleState, CpuLocalOwnerBorrow, CpuRemote};
pub(crate) use remote::{
    CpuRemotePublication, CpuRunQueueState, DeadlineBaseGuardSource, EqualRtWakeAction,
    IdlePullReservation, KtimerClaimClass, OwnerRqEnqueue, PreparedMigrationDelivery,
    RunQueueGuardSource, SchedulerRequestClaim, WakePreemptionContext, WakePreemptionDecision,
};
#[cfg(any(test, all(axtest, feature = "axtest")))]
pub(crate) use remote::{reset_rt_bandwidth_lock_acquisitions, rt_bandwidth_lock_acquisitions};
pub use snapshot::CpuSnapshot;
pub(in crate::system) use transaction::OwnerRqTaskState;
pub(crate) use transaction::{OwnerRqEntry, OwnerRqTxn, RqSwitchBaton};

use crate::{
    ActiveSchedulingState, CpuId, CpuSet, FairMode, QueuedThread, RootRtBandwidth, RqTaskMetadata,
    RtPriority, RtRunQueueBandwidth, RunQueue, SchedulePolicy, SchedulingEntity, TaskError,
    TaskSystemConfig, ThreadId, ThreadState,
    inbox::{InboxKind, InboxMessage, InboxNode, PublishResult, SchedulerInbox},
    lock::{IrqOwner, IrqScope, IrqTicketGuard, IrqTicketLock, RawTicketBaton},
    runtime::{
        AddressSpaceMembarrierState, MonotonicDeadline, MonotonicInstant, RuntimeCpuId,
        RuntimeStatus, SchedulerDeadlineUpdate, task_runtime,
    },
    thread::ThreadCore,
    timer::{
        ExpiredTaskDeadline, HardKernelTimerAction, KernelTimerEntry, KernelTimerExecution,
        KernelTimerQueue, TaskDeadlineExpireBatch, TaskDeadlineExpireRequest, TaskDeadlineQueue,
        TaskDeadlineRegistration,
    },
};
