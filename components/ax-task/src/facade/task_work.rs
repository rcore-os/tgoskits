use super::*;

pub(crate) fn publish_deferred_coroutine_reclaim(header: Pin<&'static CoroutineHeader>) {
    let system = runtime_task_system().unwrap_or_else(|_| {
        task_runtime::fatal_invariant(0x4558_0008, header.address());
    });
    match system.publish_deferred_coroutine_reclaim(header) {
        PublishResult::Published => {}
        PublishResult::AlreadyPending | PublishResult::WrongKind => {
            task_runtime::fatal_invariant(0x4558_0004, header.address());
        }
    }
}

/// Notifies the resource reaper after a runtime drops the last active-mm lease.
///
/// This allocation-free publication is valid from the IRQ-off switch tail and
/// carries no OS callback or address-space pointer.
pub fn notify_address_space_reclaim() {
    if let Ok(system) = runtime_task_system() {
        system.publish_resource_release_ready();
    }
}

/// Creates the shutdown-lifetime service thread for callbacks and reclamation.
///
/// A runtime must call this once after publishing its primary scheduler CPU and
/// before allowing ordinary application threads to exit. The service is the
/// only consumer of deferred Deadline, exit, and destruction work.
pub fn start_deferred_task_work_service() -> Result<(), TaskError> {
    let system = runtime_task_system()?;
    system.begin_task_work_worker_install()?;
    let worker =
        match ThreadBuilder::new(String::from("ax-task-reaper")).spawn(task_work_service_entry) {
            Ok(worker) => worker,
            Err(error) => {
                system.cancel_task_work_worker_install();
                return Err(error);
            }
        };
    worker.detach_permanent();
    Ok(())
}

fn task_work_service_entry() {
    if task_work_service_loop().is_err() {
        task_runtime::fatal_invariant(0x4558_0030, 0);
    }
}

fn task_work_service_loop() -> Result<(), TaskError> {
    const BATCH_LIMIT: usize = 64;

    let system = runtime_task_system()?;
    let doorbell = system.task_work_doorbell();
    let wake_owner = current_thread_handle()?.wake_handle();
    let waiter = super::irq_worker::IrqWorkerWaiter::new(wake_owner);
    system.finish_task_work_worker_install();

    loop {
        if let Some(claim) = doorbell.claim_pending() {
            debug_assert_ne!(claim.epoch(), 0);
        }
        let batch = service_task_work_pass(system, &doorbell, BATCH_LIMIT)?;
        let pending_after_pass = doorbell.claim_pending().is_some();
        match task_work_service_action(batch, pending_after_pass, BATCH_LIMIT) {
            TaskWorkServiceAction::Yield => {
                let _decision = yield_current_cpu()?;
                continue;
            }
            TaskWorkServiceAction::Wait => {
                waiter.wait(doorbell.event())?;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TaskWorkServiceAction {
    Yield,
    Wait,
}

pub(super) fn task_work_service_action(
    batch: Option<crate::DeferredTaskWorkBatch>,
    pending_after_pass: bool,
    limit: usize,
) -> TaskWorkServiceAction {
    let action = match batch {
        None => TaskWorkServiceAction::Yield,
        Some(batch) if batch.saturated(limit) || pending_after_pass => TaskWorkServiceAction::Yield,
        Some(_) => TaskWorkServiceAction::Wait,
    };
    #[cfg(feature = "qperf-metrics")]
    match action {
        TaskWorkServiceAction::Yield => crate::metrics::record_task_work_worker_yield(),
        TaskWorkServiceAction::Wait => crate::metrics::record_task_work_worker_wait(),
    }
    action
}

pub(super) fn service_task_work_pass(
    system: &TaskSystem,
    doorbell: &crate::task_work::TaskWorkDoorbell,
    limit: usize,
) -> Result<Option<crate::DeferredTaskWorkBatch>, TaskError> {
    match system.service_deferred_task_work(limit) {
        Ok(batch) => {
            #[cfg(feature = "qperf-metrics")]
            crate::metrics::record_task_work_worker_pass(batch.processed());
            Ok(Some(batch))
        }
        Err(TaskError::ThreadBusy) => {
            doorbell.reassert_pending();
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Detaches one IRQ waiter and acquires the end of every in-flight notification.
///
/// Linux PREEMPT_RT waits for non-hard irq-work completion through a sleepable
/// completion edge. The IRQ cell follows the same ownership rule: a waiter
/// that observes `Notifying` prepares a generation-checked scheduler park, and
/// the notifier publishes `Draining` before issuing the completion wake. Only
/// the hard notifier remains bounded and non-sleeping.
///
/// Callers must invoke this in task context before reusing or releasing storage
/// reachable through the matching [`IrqWaitRegistration`]. Hard-IRQ teardown
/// must instead move the token or its drain state to a task-context worker.
pub fn quiesce_irq_wait(token: IrqWaitToken<'_>) -> Result<(), TaskError> {
    validate_task_context()?;
    let mut drain = token.detach();
    loop {
        if drain.is_quiescent() {
            drain
                .try_finish()
                .expect("a quiescent IRQ wait drain must finish");
            return Ok(());
        }

        let park = match begin_current_park()? {
            CurrentParkStart::Notified => continue,
            CurrentParkStart::Prepared(park) => park,
        };
        match drain.try_finish() {
            Ok(()) => {
                park.cancel()?;
                return Ok(());
            }
            Err(in_flight) => {
                drain = in_flight;
                let _resume = park.commit()?;
            }
        }
    }
}
