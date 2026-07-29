use super::*;

pub(crate) fn publish_deferred_reclaim(node: Pin<&'static DeferredReclaimNode>, data: usize) {
    let Ok(system) = runtime_task_system() else {
        // Runtime handles remain published until shutdown. A wake released after
        // teardown cannot safely free in its current context, so leaking the
        // already-inert header is the only UAF-free shutdown fallback.
        return;
    };
    match system.publish_deferred_reclaim(node, data) {
        PublishResult::Published => {}
        PublishResult::AlreadyPending | PublishResult::WrongKind => {
            task_runtime::fatal_invariant(0x4558_0004, data);
        }
    }
}

pub(crate) fn drain_deferred_reclaims(limit: usize) -> Result<usize, TaskError> {
    runtime_task_system()?.drain_deferred_reclaims(limit)
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
    let waiter = Box::leak(Box::new(TaskWorkWaiter {
        registration: IrqWaitRegistration::new(wake_owner),
        park: WaitQueue::new(),
    }));
    system.finish_task_work_worker_install();

    loop {
        let _published = doorbell.take_pending();
        let batch = service_task_work_pass(system, &doorbell, BATCH_LIMIT)?;
        let pending_after_pass = doorbell.take_pending();
        match task_work_service_action(batch, pending_after_pass, BATCH_LIMIT) {
            TaskWorkServiceAction::Yield => {
                let _decision = yield_current_cpu()?;
                continue;
            }
            TaskWorkServiceAction::Wait => {
                wait_for_task_work(doorbell.event(), &waiter.registration, &waiter.park)?;
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
    match batch {
        None => TaskWorkServiceAction::Yield,
        Some(batch) if batch.saturated(limit) || pending_after_pass => TaskWorkServiceAction::Yield,
        Some(_) => TaskWorkServiceAction::Wait,
    }
}

pub(super) fn service_task_work_pass(
    system: &TaskSystem,
    doorbell: &crate::task_work::TaskWorkDoorbell,
    limit: usize,
) -> Result<Option<crate::DeferredTaskWorkBatch>, TaskError> {
    match system.service_deferred_task_work(limit) {
        Ok(batch) => Ok(Some(batch)),
        Err(TaskError::ThreadBusy) => {
            doorbell.reassert_pending();
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

struct TaskWorkWaiter {
    registration: IrqWaitRegistration,
    park: WaitQueue,
}

fn wait_for_task_work(
    event: &IrqWaitCell,
    registration: &IrqWaitRegistration,
    park: &WaitQueue,
) -> Result<(), TaskError> {
    match event.register(registration) {
        IrqRegisterResult::Occupied => Err(TaskError::InvalidConfiguration),
        IrqRegisterResult::ConsumedPending => Ok(()),
        IrqRegisterResult::Registered(token) | IrqRegisterResult::NotificationInFlight(token) => {
            let wait = park.try_wait_until(|| !token.is_attached());
            quiesce_irq_wait(event, token)?;
            wait
        }
    }
}

/// Detaches one IRQ waiter and yields until every in-flight notifier has
/// stopped reading its registration and wake payload.
///
/// Callers must invoke this in schedulable task context before reusing or
/// releasing storage reachable through the matching
/// [`IrqWaitRegistration`]. Hard-IRQ teardown must instead defer the token to a
/// task-context worker.
pub fn quiesce_irq_wait<'cell, 'registration>(
    event: &'cell IrqWaitCell,
    mut token: IrqWaitToken<'cell, 'registration>,
) -> Result<(), TaskError> {
    match event.unregister(&token) {
        IrqUnregisterResult::Detached | IrqUnregisterResult::NotificationInFlight => {}
        IrqUnregisterResult::Stale => return Err(TaskError::InvalidConfiguration),
    }
    loop {
        match token.try_quiesce() {
            Ok(()) => return Ok(()),
            Err(in_flight) => {
                token = in_flight;
                let _decision = yield_current_cpu()?;
            }
        }
    }
}
