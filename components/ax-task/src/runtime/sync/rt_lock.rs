//! Saved outer task state for a contended RT lock acquisition.

use alloc::sync::Arc;
use core::marker::PhantomData;

use crate::{
    runtime::{context::runtime_task_system, lock::PreemptScope},
    thread::{TaskError, ThreadCore, current::current_thread_core_arc},
};

pub(crate) struct RtLockWaitGuard {
    current: Arc<ThreadCore>,
    _not_send: PhantomData<*mut ()>,
}

impl RtLockWaitGuard {
    pub(crate) fn enter() -> Result<Self, TaskError> {
        let _preempt = PreemptScope::enter();
        let current = current_thread_core_arc()?;
        runtime_task_system()?.enter_rt_lock_wait(&current)?;
        Ok(Self {
            current,
            _not_send: PhantomData,
        })
    }
}

impl Drop for RtLockWaitGuard {
    fn drop(&mut self) {
        let _preempt = PreemptScope::enter();
        runtime_task_system()
            .and_then(|system| system.restore_rt_lock_wait(&self.current))
            .expect("RT lock acquisition must finish its inner park before restoring state");
    }
}
