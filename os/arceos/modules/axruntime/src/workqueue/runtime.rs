#[cfg(feature = "workqueue")]
static RUNTIME_WORKQUEUE: WorkQueueSystem<{ crate::CPU_CAPACITY }> = WorkQueueSystem::new();

#[cfg(feature = "workqueue")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublishedWorkerWake {
    ProgressGuaranteed,
    WorkerInvariantLost(ax_task::WakeResult),
}

#[cfg(feature = "workqueue")]
fn classify_published_worker_wake(result: ax_task::WakeResult) -> PublishedWorkerWake {
    match result {
        ax_task::WakeResult::Notified | ax_task::WakeResult::AlreadyPending => {
            PublishedWorkerWake::ProgressGuaranteed
        }
        ax_task::WakeResult::Exited | ax_task::WakeResult::Unavailable => {
            PublishedWorkerWake::WorkerInvariantLost(result)
        }
    }
}

#[cfg(feature = "workqueue")]
fn enforce_published_worker_progress(lane: &WorkerLane, result: ax_task::WakeResult) {
    match classify_published_worker_wake(result) {
        PublishedWorkerWake::ProgressGuaranteed => {}
        PublishedWorkerWake::WorkerInvariantLost(result) => {
            lane.poison_after_published_wake_failure(result);
        }
    }
}

#[cfg(feature = "workqueue")]
impl WorkQueue {
    /// Submits one item to this domain's fixed per-CPU shared worker.
    ///
    /// This is the runtime hard-IRQ-safe facade: worker readiness is checked
    /// before touching item state; the accepted path contains only atomics, one
    /// intrusive MPSC publication, and one direct scheduler wake.
    pub fn queue_work_on(
        self: Pin<&'static Self>,
        work: Pin<&'static WorkItem>,
    ) -> Result<QueueWorkResult, WorkQueueError> {
        let queue = self.get_ref();
        let route = WorkerRoute::new(queue.cpu, queue.priority, crate::CPU_CAPACITY)?;
        let lane = RUNTIME_WORKQUEUE.lane(route)?;
        let worker_wake = lane.worker_wake_handle()?;

        queue.reserve_item()?;
        if let Err(error) = work.get_ref().bind_domain(queue) {
            queue.release_item_reservation();
            return Err(error);
        }
        let result = match RUNTIME_WORKQUEUE.queue_work_on(queue.cpu, queue.priority, work) {
            Ok(result) => result,
            Err(error) => {
                queue.release_item_reservation();
                return Err(error);
            }
        };

        if result == QueueWorkResult::Queued {
            let wake_result = worker_wake.wake();
            enforce_published_worker_progress(lane, wake_result);
        } else {
            // The item was already active, so the provisional idle-item
            // reservation is not part of this domain's drain boundary.
            queue.release_item_reservation();
        }
        Ok(result)
    }

    /// Waits in task context for all activations accepted before this call.
    pub fn flush_work(
        self: Pin<&'static Self>,
        work: Pin<&'static WorkItem>,
    ) -> Result<(), WorkQueueError> {
        if work.is_idle() && work.get_ref().is_unbound() {
            return Ok(());
        }
        ensure_runtime_item_owner(self.get_ref(), work.get_ref())?;
        let token = RUNTIME_WORKQUEUE.begin_flush(work);
        wait_for_runtime_completion(work.get_ref(), RuntimeCompletion::Flush(token))
    }

    /// Cancels queued/rerun work and waits in task context for callback exit.
    pub fn cancel_work_sync(
        self: Pin<&'static Self>,
        work: Pin<&'static WorkItem>,
    ) -> Result<(), WorkQueueError> {
        if work.is_idle() && work.get_ref().is_unbound() {
            return Ok(());
        }
        ensure_runtime_item_owner(self.get_ref(), work.get_ref())?;
        ensure_runtime_wait_context(work.get_ref())?;
        let token = RUNTIME_WORKQUEUE.begin_cancel(work);
        wait_for_runtime_completion(work.get_ref(), RuntimeCompletion::Cancel(token))
    }

    /// Stops admission and waits until every item accepted by this logical
    /// domain reaches idle.
    ///
    /// The fixed worker pool remains available: draining a logical domain does
    /// not tear down either shared per-CPU worker. This task-context operation
    /// must run outside every fixed worker callback, because an accepted item
    /// may require that same lane to complete. Context validation precedes the
    /// admission transition, including when the domain is already empty.
    pub fn drain_workqueue(self: Pin<&'static Self>) -> Result<(), WorkQueueError> {
        ensure_runtime_domain_wait_context()?;
        let token = self.begin_drain()?;
        if token.is_complete() {
            return Ok(());
        }
        self.drain_wait.try_wait_until(|| token.is_complete())?;
        Ok(())
    }
}

#[cfg(feature = "workqueue")]
#[derive(Clone, Copy)]
enum RuntimeCompletion {
    Flush(FlushToken),
    Cancel(CancelToken),
}

#[cfg(feature = "workqueue")]
impl RuntimeCompletion {
    fn is_complete(self) -> bool {
        match self {
            Self::Flush(token) => token.is_complete(),
            Self::Cancel(token) => token.is_complete(),
        }
    }
}

#[cfg(feature = "workqueue")]
fn ensure_runtime_item_owner(queue: &WorkQueue, work: &WorkItem) -> Result<(), WorkQueueError> {
    if !work.belongs_to_domain(queue) {
        return Err(WorkQueueError::ForeignDomain);
    }
    if !work.belongs_to_system(&RUNTIME_WORKQUEUE) {
        return Err(WorkQueueError::ForeignSystem);
    }
    Ok(())
}

#[cfg(feature = "workqueue")]
fn ensure_runtime_wait_context(work: &WorkItem) -> Result<(), WorkQueueError> {
    let current = ensure_runtime_domain_wait_context()?;
    if work.executing_worker.load(Ordering::Acquire) == current {
        return Err(WorkQueueError::WouldDeadlock);
    }
    let state = work.state.load(Ordering::Acquire);
    if let Some(route) = state_route(state)
        && RUNTIME_WORKQUEUE.lane(route)?.worker_id() == current
    {
        return Err(WorkQueueError::WouldDeadlock);
    }
    Ok(())
}

#[cfg(feature = "workqueue")]
fn ensure_runtime_domain_wait_context() -> Result<u64, WorkQueueError> {
    if ax_hal::irq::in_irq_context()
        || crate::guard::validate_schedule_context(ax_task::runtime::RuntimeScheduleOrigin::Block)
            != ax_task::runtime::RuntimeStatus::Success
    {
        return Err(WorkQueueError::UnsafeContext);
    }
    let current = current_thread_id()?.as_u64();
    if RUNTIME_WORKQUEUE.owns_worker_thread(current) {
        return Err(WorkQueueError::WouldDeadlock);
    }
    Ok(current)
}

#[cfg(feature = "workqueue")]
fn wait_for_runtime_completion(
    work: &'static WorkItem,
    completion: RuntimeCompletion,
) -> Result<(), WorkQueueError> {
    ensure_runtime_wait_context(work)?;
    if completion.is_complete() {
        return Ok(());
    }
    work.completion_wait
        .try_wait_until(|| completion.is_complete())?;
    Ok(())
}

/// Installs this online CPU's fixed normal and high-priority shared workers.
///
/// The caller must invoke this from ordinary task context, with local IRQs and
/// preemption available, after publishing the scheduler CPU online and before
/// enabling any producer that can submit work on that CPU. The function may
/// yield while another initializer finishes and returns only after both
/// shutdown-lifetime wake handles are visible through Acquire loads. It
/// intentionally is not wired into `rust_main` here; platform startup owns the
/// final ordering relative to IRQ and device enablement.
#[cfg(feature = "workqueue")]
pub fn initialize_workqueue_cpu(cpu: usize) -> Result<(), WorkQueueError> {
    if crate::guard::validate_schedule_context(ax_task::runtime::RuntimeScheduleOrigin::Yield)
        != ax_task::runtime::RuntimeStatus::Success
    {
        return Err(WorkQueueError::UnsafeContext);
    }
    let current = ax_hal::percpu::this_cpu_id();
    if current != cpu {
        return Err(WorkQueueError::CpuInitializationOwner {
            expected: cpu,
            actual: current,
        });
    }
    install_runtime_worker(WorkerRoute {
        cpu,
        priority: WorkPriority::Normal,
    })?;
    install_runtime_worker(WorkerRoute {
        cpu,
        priority: WorkPriority::High,
    })?;

    for priority in [WorkPriority::Normal, WorkPriority::High] {
        let route = WorkerRoute { cpu, priority };
        let lane = RUNTIME_WORKQUEUE.lane(route)?;
        while !lane.worker_ready() {
            let _decision = yield_current_cpu()?;
        }
    }
    Ok(())
}

/// Installs the primary CPU workers and verifies that every runtime CPU has
/// already published both fixed lanes.
///
/// Secondary CPUs install their own workers before publishing CPU-runtime
/// readiness. Therefore this function never creates remote workers and never
/// waits for a CPU that still lacks a scheduler/IRQ execution context.
#[cfg(feature = "workqueue")]
pub fn initialize() -> Result<(), WorkQueueError> {
    initialize_workqueue_cpu(ax_hal::percpu::this_cpu_id())?;
    for cpu in 0..crate::runtime_cpu_count() {
        for priority in [WorkPriority::Normal, WorkPriority::High] {
            if !RUNTIME_WORKQUEUE
                .lane(WorkerRoute { cpu, priority })?
                .worker_ready()
            {
                return Err(WorkQueueError::WorkerNotInitialized);
            }
        }
    }
    Ok(())
}

#[cfg(feature = "workqueue")]
fn install_runtime_worker(route: WorkerRoute) -> Result<(), WorkQueueError> {
    let route = WorkerRoute::new(route.cpu, route.priority, crate::CPU_CAPACITY)?;
    let lane = RUNTIME_WORKQUEUE.lane(route)?;
    loop {
        match lane.begin_worker_install() {
            Ok(true) => break,
            Ok(false) => return Ok(()),
            Err(WorkQueueError::WorkerInstalling) => {
                let _decision = yield_current_cpu()?;
            }
            Err(error) => return Err(error),
        }
    }

    let topology = ax_hal::cpu_num();
    let cpu = match u32::try_from(route.cpu) {
        Ok(cpu) => cpu,
        Err(_) => {
            lane.cancel_worker_install();
            return Err(WorkQueueError::InvalidCpu {
                cpu: route.cpu,
                cpu_count: topology,
            });
        }
    };
    let mut affinity = CpuSet::empty(topology);
    if !affinity.insert(CpuId::new(cpu)) {
        lane.cancel_worker_install();
        return Err(WorkQueueError::InvalidCpu {
            cpu: route.cpu,
            cpu_count: topology,
        });
    }
    let nice = match route.priority {
        WorkPriority::Normal => Nice::ZERO,
        WorkPriority::High => match Nice::new(-10) {
            Ok(nice) => nice,
            Err(error) => {
                lane.cancel_worker_install();
                return Err(error.into());
            }
        },
    };
    let policy = SchedulePolicy::fair(nice, FairMode::Normal);
    let class = match route.priority {
        WorkPriority::Normal => "normal",
        WorkPriority::High => "highpri",
    };
    let thread = match crate::task::spawn_kernel_worker(
        move || runtime_worker_entry(route),
        format!("ax-wq/{}/{class}", route.cpu),
        affinity,
        policy,
    ) {
        Ok(thread) => thread,
        Err(error) => {
            lane.cancel_worker_install();
            return Err(error.into());
        }
    };
    lane.publish_worker(&thread);
    drop(thread);
    Ok(())
}

#[cfg(feature = "workqueue")]
fn runtime_worker_entry(route: WorkerRoute) {
    if let Err(error) = runtime_worker_loop(route) {
        panic!("fixed workqueue worker failed: {error}");
    }
}

#[cfg(feature = "workqueue")]
fn runtime_worker_loop(route: WorkerRoute) -> Result<(), WorkQueueError> {
    let lane = RUNTIME_WORKQUEUE.lane(route)?;
    loop {
        let batch = RUNTIME_WORKQUEUE.service_runtime_batch(route.cpu, route.priority)?;
        if batch.saturated() {
            let _decision = yield_current_cpu()?;
        }
        if lane.has_pending() {
            continue;
        }
        lane.worker_park.try_wait_until(|| lane.has_pending())?;
    }
}

#[cfg(all(test, feature = "workqueue"))]
mod runtime_tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    #[test]
    fn post_publication_wake_classification_never_treats_worker_loss_as_admission_failure() {
        for progress in [
            ax_task::WakeResult::Notified,
            ax_task::WakeResult::AlreadyPending,
        ] {
            assert_eq!(
                classify_published_worker_wake(progress),
                PublishedWorkerWake::ProgressGuaranteed
            );
        }
        for lost in [
            ax_task::WakeResult::Exited,
            ax_task::WakeResult::Unavailable,
        ] {
            assert_eq!(
                classify_published_worker_wake(lost),
                PublishedWorkerWake::WorkerInvariantLost(lost)
            );
        }
    }

    #[test]
    fn post_publication_worker_loss_permanently_poisoned_the_lane() {
        let lane = WorkerLane::new();

        let fatal = catch_unwind(AssertUnwindSafe(|| {
            lane.poison_after_published_wake_failure(ax_task::WakeResult::Unavailable)
        }));

        assert!(fatal.is_err());
        assert!(lane.worker_poisoned.load(Ordering::Acquire));
        assert!(lane.doorbell.load(Ordering::SeqCst));
        assert!(matches!(
            lane.worker_wake_handle(),
            Err(WorkQueueError::WorkerPoisoned)
        ));
    }
}
