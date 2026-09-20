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
pub(crate) use dispatch::{
    CurrentDispatch, CurrentRemotePublication, DispatchCharge, DispatchRole,
    PreviousSwitchDisposition, PreviousSwitchOwnership, SchedulerPolicyRef, SchedulerThreadRef,
    SwitchHandoff,
};
pub use load::{CpuLoadSummary, DeadlineBandwidthSnapshot, SchedulingClass};
use load::{SUMMARY_FAIR_IDLE_ONLY, SUMMARY_FAIR_PUSHABLE};
pub use local::CpuLocal;
pub(crate) use local::{
    HardTimerServiceClaim, HardTimerServiceStep, KtimerServiceClaim,
    SchedulerDeadlineDerivationSource, SchedulerDeadlineRqObservation, SoftTimerExpireBatch,
};
use remote::RqCurrentUpdate;
pub use remote::{CpuLifecycleState, CpuLocalOwnerBorrow, CpuRemote};
pub(crate) use remote::{
    CpuRemotePublication, CpuRunQueueState, DeadlineBaseGuardSource, EqualRtWakeAction,
    IdlePullReservation, KtimerClaimClass, OwnerRqEnqueue, PreparedMigrationDelivery,
    RescheduleKind, RunQueueDomainPublication, RunQueueGuardSource, SchedulerRequestClaim,
    SchedulerRequestScope, WakePreemptionContext, WakePreemptionDecision,
};
pub use snapshot::CpuSnapshot;
pub(in crate::sched::system) use transaction::OwnerRqTaskState;
pub(crate) use transaction::{OwnerRqEntry, OwnerRqTxn, RqSwitchBaton};

use crate::{
    runtime::{
        RuntimeStatus,
        config::TaskSystemConfig,
        cpu::{RuntimeCpuId, SchedulerDeadlineUpdate, SchedulerRuntimeDeadline},
        delivery::inbox::{InboxKind, InboxMessage, InboxNode, PublishResult, SchedulerInbox},
        lock::{IrqOwner, IrqScope, IrqTicketGuard, IrqTicketLock, RawTicketBaton},
        resource::AddressSpaceMembarrierState,
        task_runtime,
    },
    sched::{
        CpuId, CpuSet, FairMode, RtPriority, SchedulePolicy,
        algorithm::{
            ActiveSchedulingState, QueuedThread, RootRtBandwidth, RqTaskMetadata,
            RtRunQueueBandwidth, RunQueue, SchedulingEntity,
        },
    },
    thread::{TaskError, ThreadCore, ThreadId, ThreadState},
    time::{
        MonotonicDeadline, MonotonicInstant,
        queue::{
            ExpiredTaskDeadline, HardKernelTimerAction, HardTaskDeadlineClaim, KernelTimerEntry,
            KernelTimerExecution, KernelTimerQueue, TaskDeadlineExpireBatch,
            TaskDeadlineExpireRequest, TaskDeadlineKind, TaskDeadlineQueue,
            TaskDeadlineRegistration,
        },
    },
};
