//! Scheduler-owned Linux membarrier protocol.

use super::*;
use crate::{
    runtime::{MembarrierRegistration, RuntimeMembarrierAction},
    system::{MembarrierCpuTargets, MembarrierTarget},
};

/// Memory-barrier target semantics implemented by the scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembarrierCommand {
    /// Orders every online CPU through one synchronous scheduler rendezvous.
    Global,
    /// Orders CPUs currently executing any globally registered user `mm`.
    GlobalExpedited,
    /// Orders CPUs currently executing the caller's shared `mm`.
    PrivateExpedited,
}

fn synchronize_targets(targets: &CpuSet, action: RuntimeMembarrierAction) -> Result<(), TaskError> {
    for cpu in targets.iter() {
        match task_runtime::synchronize_membarrier_cpu(RuntimeCpuId::new(cpu.as_u32()), action) {
            RuntimeStatus::Success => {}
            status => return Err(TaskError::RuntimeFailure(status as u32)),
        }
    }
    Ok(())
}

/// Registers one irreversible expedited facility for the caller's shared `mm`.
pub fn register_current_membarrier(registration: MembarrierRegistration) -> Result<(), TaskError> {
    validate_task_context()?;
    let system = runtime_task_system()?;
    let targets = MembarrierCpuTargets::new(system.cpu_topology_len());
    let _pin = PreemptScope::enter();
    let plan = {
        let mut irq = RuntimeIrqGuard::enter();
        let mut cpu = runtime_current_cpu_mut(&mut irq)?;
        system.begin_current_membarrier_registration(cpu.as_mut(), registration, targets)?
    };
    synchronize_targets(plan.targets(), RuntimeMembarrierAction::RefreshRunQueue)?;
    system.complete_membarrier_registration(plan);
    Ok(())
}

/// Executes one Linux-style membarrier command.
pub fn membarrier(command: MembarrierCommand) -> Result<(), crate::MembarrierError> {
    validate_task_context()?;
    let system = runtime_task_system()?;
    let targets = MembarrierCpuTargets::new(system.cpu_topology_len());
    let _pin = PreemptScope::enter();

    // Matches the full barrier before Linux scans rq->curr.
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    let targets = {
        let mut irq = RuntimeIrqGuard::enter();
        let mut cpu = runtime_current_cpu_mut(&mut irq)?;
        let target = match command {
            MembarrierCommand::Global => MembarrierTarget::Global,
            MembarrierCommand::GlobalExpedited => MembarrierTarget::GlobalExpedited,
            MembarrierCommand::PrivateExpedited => {
                system.current_private_membarrier_target(cpu.as_mut())?
            }
        };
        system.current_membarrier_targets(cpu.as_mut(), target, targets)?
    };
    synchronize_targets(targets.cpus(), RuntimeMembarrierAction::MemoryBarrier)?;
    // Matches the full barrier after Linux waits for the final target IPI.
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    Ok(())
}

/// Refreshes the calling CPU's `rq->membarrier_state` from its current task.
///
/// This fixed entry is invoked only by the runtime's registration hard-call.
#[doc(hidden)]
pub fn refresh_current_membarrier_run_queue() -> Result<(), TaskError> {
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    runtime_task_system()?.refresh_current_membarrier_run_queue(cpu.as_mut())
}
