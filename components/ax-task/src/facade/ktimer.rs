//! PREEMPT_RT-style per-CPU `ktimers/%u` service threads.

use super::*;

/// Creates the current CPU's shutdown-lifetime soft-timer worker.
///
/// The runtime calls this exactly once after the rq becomes online and before
/// enabling local timer IRQs. The thread is bound to that CPU, runs at Linux's
/// low FIFO kernel-worker priority, and sleeps on the CPU's sticky IRQ event.
pub fn start_current_ktimer_service() -> Result<(), TaskError> {
    validate_task_context()?;
    let system = runtime_task_system()?;
    let remote = {
        let _irq = RuntimeIrqGuard::enter();
        let remote = current_cpu_remote().ok_or(TaskError::NotInitialized)?;
        remote.begin_ktimer_worker_install()?;
        remote
    };
    let owner = remote.owner();
    let mut affinity = CpuSet::empty(system.cpu_topology_len());
    if !affinity.insert(owner) {
        remote.cancel_ktimer_worker_install();
        return Err(TaskError::InvalidConfiguration);
    }
    let policy = SchedulePolicy::fifo(
        RtPriority::new(1).expect("Linux low FIFO priority must remain representable"),
    );
    let worker = match ThreadBuilder::new(alloc::format!("ktimers/{}", owner.as_u32()))
        .policy(policy)
        .affinity(affinity)
        .spawn(move || ktimer_service_entry(owner))
    {
        Ok(worker) => worker,
        Err(error) => {
            remote.cancel_ktimer_worker_install();
            return Err(error);
        }
    };
    worker.detach_permanent();
    Ok(())
}

fn current_ktimer_remote(expected: CpuId) -> Result<&'static CpuRemote, TaskError> {
    let _irq = RuntimeIrqGuard::enter();
    let remote = current_cpu_remote().ok_or(TaskError::NotInitialized)?;
    if remote.owner() != expected {
        return Err(TaskError::CpuOwnerMismatch {
            expected: expected.as_u32(),
            actual: remote.owner().as_u32(),
        });
    }
    Ok(remote)
}

fn ktimer_service_entry(owner: CpuId) {
    if ktimer_service_loop(owner).is_err() {
        task_runtime::fatal_invariant(0x4b54_0030, owner.as_u32() as usize);
    }
}

fn ktimer_service_loop(owner: CpuId) -> Result<(), TaskError> {
    let remote = current_ktimer_remote(owner)?;
    let current = current_thread_handle()?;
    let waiter = super::irq_worker::IrqWorkerWaiter::new(current.wake_handle());
    remote.finish_ktimer_worker_install(current.id());

    loop {
        let Some(claim) = remote.claim_ktimer_work() else {
            waiter.wait(remote.ktimer_event())?;
            continue;
        };
        let pass = service_current_ktimer_pass(owner);
        remote.complete_ktimer_work(claim);
        if pass? {
            let _decision = yield_current_cpu()?;
        }
    }
}

fn service_current_ktimer_pass(owner: CpuId) -> Result<bool, TaskError> {
    let system = runtime_task_system()?;
    let (pending, mut kernel_timer, mut task_timer) = {
        let mut irq = RuntimeIrqGuard::enter();
        let mut cpu = runtime_current_cpu_mut(&mut irq)?;
        if cpu.owner() != owner {
            return Err(TaskError::CpuOwnerMismatch {
                expected: owner.as_u32(),
                actual: cpu.owner().as_u32(),
            });
        }
        let mut batch = system.service_ktimer_work(cpu.as_mut())?;
        if let Some(update) = batch.update() {
            task_runtime::publish_scheduler_deadline(update);
        }
        (
            batch.pending(),
            batch.take_kernel_timer(),
            batch.take_task_timer(),
        )
    };
    if let Some(event) = task_timer.take() {
        let handle = match event.thread().map(|thread| system.thread_handle(thread)) {
            Some(Ok(handle)) => Some(handle),
            Some(Err(TaskError::StaleThreadId)) | None => None,
            Some(Err(_)) => task_runtime::fatal_invariant(
                0x4b54_0031,
                event.thread().map_or(0, ThreadId::as_u64) as usize,
            ),
        };
        let (wake, update) = {
            let mut irq = RuntimeIrqGuard::enter();
            // Once an expiration leaves the deadline base, this worker owns
            // its only completion token. Losing the pinned CPU or returning a
            // recoverable error would orphan that token.
            let mut cpu = runtime_current_cpu_mut(&mut irq).unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x4b54_0032, owner.as_u32() as usize)
            });
            if cpu.owner() != owner {
                task_runtime::fatal_invariant(0x4b54_0033, cpu.owner().as_u32() as usize);
            }
            system
                .complete_task_timer_execution(cpu.as_mut(), event, handle.as_ref())
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x4b54_0034, owner.as_u32() as usize)
                })
        };
        if let Some(update) = update {
            task_runtime::publish_scheduler_deadline(update);
        }
        if let Some(wake) = wake {
            let _wake_result = wake.wake();
        }
    }
    if let Some(mut timer) = kernel_timer.take() {
        let action = timer.invoke();
        let mut completion = {
            let mut irq = RuntimeIrqGuard::enter();
            // The callback entry and its queue tombstone must complete as one
            // ownership transaction after arbitrary callback code returns.
            let mut cpu = runtime_current_cpu_mut(&mut irq).unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x4b54_0035, owner.as_u32() as usize)
            });
            if cpu.owner() != owner {
                task_runtime::fatal_invariant(0x4b54_0036, cpu.owner().as_u32() as usize);
            }
            system
                .complete_kernel_timer_execution(cpu.as_mut(), timer, action)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x4b54_0037, owner.as_u32() as usize)
                })
        };
        if let Some(update) = completion.update() {
            task_runtime::publish_scheduler_deadline(update);
        }
        drop(completion.take_completed());
    }
    Ok(pending)
}
