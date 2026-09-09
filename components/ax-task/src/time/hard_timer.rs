//! Explicitly IRQ-safe, restartable timer callbacks.

pub use crate::time::{
    queue::{
        HardKernelTimerAction, HardKernelTimerCallback, HardKernelTimerHandle,
        HardRestartableKernelTimerCallback,
    },
    registration::{
        arm_hard_kernel_timer, disarm_hard_kernel_timer, register_hard_restartable_kernel_timer,
    },
};
