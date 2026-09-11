//! Thread identity, lifecycle and generation-valid management handles.

pub mod current;
mod current_token;
mod handle;
mod id;
mod park;
pub(crate) mod pi;
mod pi_tree;

pub(crate) mod spec;
mod state;
mod state_kind;
pub(crate) mod tick_work;

pub use current_token::CurrentThreadToken;
pub(crate) use handle::{OwnedThreadSchedulerExit, ThreadCore, ThreadCoreInit, WakeIntent};
pub use handle::{
    ThreadHandle, ThreadRuntimeSnapshot, ThreadWakeBatch, ThreadWakeHandle, WakeResult,
    WeakThreadHandle,
};
pub use id::ThreadId;
pub use park::{ParkCommit, ParkPrepare, ParkTicket};
pub(crate) use park::{WaitWakeClaim, WaitWakeClaimState, WaitWakeDelivery};
pub(crate) use pi::{
    PiMutexWaiters, PiWaitRegistration, PiWaitState, drop_pi_mutex_wait_handle,
    lock_pi_mutex_waiters, lock_raw_pi_mutex_waiters, try_lock_raw_pi_mutex_waiters,
};
pub(crate) use pi_tree::*;
pub use spec::{
    RunningPolicyAppliedHook, SwitchReason, ThreadExtension, ThreadExtensionBorrow,
    ThreadExtensionLease, ThreadExtensionOps, ThreadExtensionView, ThreadSpec,
};
pub(crate) use state::{ParkPublication, ThreadLifecycle, WakePublication, transition_is_valid};
pub use state_kind::ThreadState;
pub(crate) use tick_work::{SchedulerTickWork, SchedulerTickWorkClaim};

pub(crate) use crate::sched::{
    affinity::ThreadAffinityCompletion,
    policy::{
        DEADLINE_CLASS_RANK, DeadlineEntity, DeadlineServer, REALTIME_CLASS_RANK, SchedulingKey,
        SchedulingUrgency,
    },
};
pub use crate::thread::{
    error::TaskError,
    spawn::{DEFAULT_KERNEL_THREAD_STACK_SIZE, ThreadBuilder},
};

pub(crate) mod error;

pub(crate) mod execution;
pub(crate) mod spawn;
pub use execution::{PreparedThread, StagedThread};

pub(crate) mod allocation;
#[cfg(feature = "fault-injection")]
pub use allocation::ThreadAllocationProbe;
