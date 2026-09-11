//! Task-context timer callbacks and shared cancellation outcomes.

pub use crate::time::{
    queue::{
        KernelTimerAction, KernelTimerCallback, KernelTimerCancelOutcome, KernelTimerHandle,
        RestartableKernelTimerCallback,
    },
    registration::{cancel_kernel_timer, register_kernel_timer, register_restartable_kernel_timer},
};
