//! One-shot wake injection into the real idle polling-to-wait boundary.

use core::marker::PhantomData;

use super::*;
use crate::{runtime::lock::PreemptTicketLock, sched::CpuId, thread::ThreadWakeHandle};

struct IdlePollingWake {
    cpu: CpuId,
    wake: Option<ThreadWakeHandle>,
}

static WAKE: PreemptTicketLock<Option<IdlePollingWake>> = PreemptTicketLock::new(None);

/// Owns one wake injected after polling is published, before the IRQ guard exits.
/// Dropping an unconsumed probe cancels the retained wake capability.
pub struct IdlePollingWakeProbe {
    _not_send: PhantomData<*mut ()>,
}

impl IdlePollingWakeProbe {
    /// Arms the next idle polling attempt on `cpu`; concurrent probes are rejected.
    ///
    /// Requires a blocking-capable task context; returns [`TaskError::UnsafeContext`]
    /// before acquiring the probe lock when called from an atomic context.
    pub fn arm(cpu: CpuId, wake: ThreadWakeHandle) -> Result<Self, TaskError> {
        crate::thread::current::validate_blocking_context()?;
        let system = crate::runtime::context::runtime_task_system()?;
        if system.cpu_remote(cpu).is_none() {
            return Err(TaskError::CpuOffline(cpu.as_u32()));
        }
        let mut pending = WAKE.lock();
        if pending.is_some() {
            return Err(TaskError::ThreadBusy);
        }
        *pending = Some(IdlePollingWake {
            cpu,
            wake: Some(wake),
        });
        Ok(Self {
            _not_send: PhantomData,
        })
    }
}

impl Drop for IdlePollingWakeProbe {
    fn drop(&mut self) {
        let cancelled = WAKE.lock().take();
        drop(cancelled);
    }
}

pub(super) fn wake_after_polling(cpu: CpuId) {
    let wake = {
        let mut pending = WAKE.lock();
        pending
            .as_mut()
            .filter(|pending| pending.cpu == cpu)
            .and_then(|pending| pending.wake.take())
    };
    if let Some(wake) = wake {
        wake.wake();
    }
}

/// Observes the real CPU polling publication from a resumed test task.
pub fn current_cpu_is_idle_polling() -> Result<bool, TaskError> {
    let _pin = PreemptScope::enter();
    Ok(current_cpu_remote()
        .ok_or(TaskError::NotInitialized)?
        .is_idle_polling())
}
