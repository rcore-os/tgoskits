//! Deferred task-context work and resource reclamation.

use super::*;
use crate::SchedulerTickWorkDisposition;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SchedulerTickDispatch {
    events: usize,
    callbacks: usize,
    retry_deferred: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceReclaim {
    None,
    Coroutine,
    AddressSpace,
}

impl TaskSystem {
    pub(crate) fn publish_current_scheduler_tick_work(
        &self,
        cpu: &CpuLocal,
        expected: ThreadId,
        observed_ns: u64,
    ) -> Result<(), TaskError> {
        let Some(core) = cpu.current_core() else {
            return Err(TaskError::NoRunnableThread);
        };
        if core.id() != expected {
            return Err(TaskError::StaleThreadId);
        }
        self.publish_scheduler_tick_work(&core, observed_ns);
        Ok(())
    }

    fn publish_scheduler_tick_work(&self, core: &Arc<ThreadCore>, observed_ns: u64) {
        if !core.begin_scheduler_tick_work(observed_ns) {
            return;
        }
        self.publish_claimed_scheduler_tick_work(core);
    }

    fn publish_claimed_scheduler_tick_work(&self, core: &Arc<ThreadCore>) {
        if !core.reserve_scheduler_inbox_delivery() {
            core.cancel_scheduler_tick_work();
            return;
        }

        let core_ptr = Arc::as_ptr(core);
        // SAFETY: the retained strong count is transferred to the task-work
        // inbox and released by its sole consumer after callback completion.
        unsafe { Arc::increment_strong_count(core_ptr) };
        // SAFETY: Arc allocations are pinned for their lifetime, and the
        // delivery reservation keeps this core and its extension alive.
        let node = unsafe { Pin::new_unchecked((&*core_ptr).scheduler_tick_work_node()) };
        let result = self.deferred_scheduler_ticks.publish(
            node,
            InboxMessage::scheduler_tick(core.id(), core_ptr.expose_provenance()),
        );
        if result == PublishResult::Published {
            self.task_work.publish();
            return;
        }

        // SAFETY: a rejected publication did not consume the transferred Arc.
        unsafe { Arc::decrement_strong_count(core_ptr) };
        core.cancel_scheduler_inbox_delivery();
        core.cancel_scheduler_tick_work();
        task_runtime::fatal_invariant(0x5457_0001, result as usize);
    }

    /// Publishes one zero-reference coroutine header from hard IRQ context.
    pub(crate) fn publish_deferred_coroutine_reclaim(
        &self,
        header: Pin<&'static CoroutineHeader>,
    ) -> PublishResult {
        let data = header.address();
        let _publication = IrqScope::enter();
        let result = self.deferred_coroutine_reclaims.publish(
            header.reclaim_node(),
            InboxMessage::reclaim(ThreadId::from_parts(0, 0), 0, data),
        );
        if result == PublishResult::Published {
            self.task_work.publish();
        }
        result
    }

    pub(crate) fn task_work_doorbell(&self) -> Arc<TaskWorkDoorbell> {
        Arc::clone(&self.task_work)
    }

    pub(crate) fn begin_task_work_worker_install(&self) -> Result<(), TaskError> {
        self.task_work.begin_worker_install()
    }

    pub(crate) fn finish_task_work_worker_install(&self) {
        self.task_work.finish_worker_install();
    }

    pub(crate) fn cancel_task_work_worker_install(&self) {
        self.task_work.cancel_worker_install();
    }

    /// Reports whether a sticky task-work publication awaits the service thread.
    pub fn deferred_task_work_pending(&self) -> bool {
        self.task_work.is_pending()
    }

    pub(crate) fn publish_resource_release_ready(&self) {
        self.task_work.publish();
    }

    /// Runs one bounded task-context pass as the single task-work consumer.
    ///
    /// Unrelated work classes are interleaved through a persistent round-robin
    /// cursor. Per-thread claim predicates still enforce Deadline callback,
    /// exit callback, and record-reaping order. A concurrent or reentrant
    /// consumer receives [`TaskError::ThreadBusy`] without consuming work.
    pub fn service_deferred_task_work(
        &self,
        limit: usize,
    ) -> Result<DeferredTaskWorkBatch, TaskError> {
        if task_runtime::in_hard_irq() {
            return Err(TaskError::UnsafeContext);
        }
        let limit = limit.min(crate::DEFAULT_BATCH_LIMIT);
        if limit == 0 {
            return Ok(DeferredTaskWorkBatch::default());
        }
        let _consumer: TaskWorkConsumerGuard<'_> = self.task_work.try_claim_consumer()?;
        let mut next_class = self.state.lock().task_work_class_cursor;
        let outcome = (|| {
            let mut batch = DeferredTaskWorkBatch::default();
            let mut classes_without_progress = 0;
            let mut scheduler_tick_retry_deferred = false;
            while batch.processed() < limit
                && classes_without_progress < DeferredTaskWorkClass::COUNT
            {
                let class = next_class;
                next_class = class.next();
                let processed = match class {
                    DeferredTaskWorkClass::Deadline => {
                        let (events, callbacks) = self.dispatch_deadline_overruns_inner(1)?;
                        batch.deadline_events += events;
                        batch.deadline_callbacks += callbacks;
                        events
                    }
                    DeferredTaskWorkClass::SchedulerTick if scheduler_tick_retry_deferred => 0,
                    DeferredTaskWorkClass::SchedulerTick => {
                        let dispatch = self.dispatch_scheduler_tick_work_inner(1)?;
                        batch.scheduler_tick_events += dispatch.events;
                        batch.scheduler_tick_callbacks += dispatch.callbacks;
                        scheduler_tick_retry_deferred |= dispatch.retry_deferred;
                        dispatch.events
                    }
                    DeferredTaskWorkClass::Exit => {
                        let callbacks = self.dispatch_exit_callbacks_inner(1)?;
                        batch.exit_callbacks += callbacks;
                        callbacks
                    }
                    DeferredTaskWorkClass::Reap => {
                        let reaped = self.reap_unreferenced_exited_inner(1)?;
                        batch.reaped_threads += reaped;
                        reaped
                    }
                    DeferredTaskWorkClass::Reclaim => match self.reclaim_one_resource()? {
                        ResourceReclaim::None => 0,
                        ResourceReclaim::Coroutine => {
                            batch.coroutine_reclaims += 1;
                            1
                        }
                        ResourceReclaim::AddressSpace => {
                            batch.address_space_reclaims += 1;
                            1
                        }
                    },
                };
                if processed == 0 {
                    classes_without_progress += 1;
                } else {
                    classes_without_progress = 0;
                }
            }
            debug_assert!(batch.processed() <= limit);
            #[cfg(feature = "qperf-metrics")]
            crate::metrics::record_task_work_classes(
                batch.deadline_events,
                batch.scheduler_tick_events,
                batch.exit_callbacks,
                batch.reaped_threads,
                batch.coroutine_reclaims,
                batch.address_space_reclaims,
            );
            Ok(batch)
        })();
        self.state.lock().task_work_class_cursor = next_class;
        outcome
    }

    fn dispatch_scheduler_tick_work_inner(
        &self,
        limit: usize,
    ) -> Result<SchedulerTickDispatch, TaskError> {
        let mut messages = [InboxMessage::EMPTY; crate::DEFAULT_BATCH_LIMIT];
        let batch = self
            .deferred_scheduler_ticks
            .drain(limit.min(crate::DEFAULT_BATCH_LIMIT), &mut messages);
        let mut callbacks = 0;
        let mut retry_deferred = false;
        for message in messages.iter().take(batch.drained()) {
            if message.operation() != InboxOperation::SchedulerTick || message.payload() == 0 {
                task_runtime::fatal_invariant(0x5457_0002, message.payload());
            }
            let core = unsafe {
                // SAFETY: publication transferred exactly one Arc strong count
                // whose pointer is carried by this detached message.
                Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                    message.payload(),
                ))
            };
            let _delivery = core.accept_scheduler_inbox_delivery();
            if core.id() != message.thread_id() {
                core.cancel_scheduler_tick_work();
                continue;
            }
            let claim = core.take_scheduler_tick_work();
            if let Some(claim) = claim
                && let Some(extension) = core.extension_view()
            {
                // SAFETY: the inbox delivery reservation prevents extension
                // reclamation even if the carrier thread exits concurrently.
                // The gate generation decides whether the process/subsystem
                // work remains relevant; carrier-thread state does not.
                let disposition = unsafe { claim.invoke(extension.data(), core.id()) };
                callbacks += 1;
                if disposition == SchedulerTickWorkDisposition::Retry {
                    retry_deferred = true;
                    if core.retry_scheduler_tick_work(&claim) {
                        self.publish_claimed_scheduler_tick_work(&core);
                    }
                }
            }
        }
        if batch.pending() {
            self.task_work.publish();
        }
        Ok(SchedulerTickDispatch {
            events: batch.drained(),
            callbacks,
            retry_deferred,
        })
    }

    fn reclaim_one_resource(&self) -> Result<ResourceReclaim, TaskError> {
        let address_space_reclaim_first = {
            let mut state = self.state.lock();
            let current = state.address_space_reclaim_first;
            state.address_space_reclaim_first = !current;
            current
        };
        if address_space_reclaim_first {
            if self.reclaim_pending_address_space() {
                return Ok(ResourceReclaim::AddressSpace);
            }
            self.drain_deferred_coroutine_reclaims_inner(1)
                .map(|count| match count {
                    0 => ResourceReclaim::None,
                    1 => ResourceReclaim::Coroutine,
                    _ => unreachable!("single-resource drain exceeded its bound"),
                })
        } else {
            let reclaimed = self.drain_deferred_coroutine_reclaims_inner(1)?;
            if reclaimed != 0 {
                Ok(ResourceReclaim::Coroutine)
            } else if self.reclaim_pending_address_space() {
                Ok(ResourceReclaim::AddressSpace)
            } else {
                Ok(ResourceReclaim::None)
            }
        }
    }

    fn reclaim_pending_address_space(&self) -> bool {
        let Some(address_space) = self.state.lock().pending_address_space_reclaims.pop() else {
            return false;
        };
        let handle = address_space.handle();
        match task_runtime::destroy_address_space(handle) {
            AddressSpaceDestroyOutcome::Released => {}
            AddressSpaceDestroyOutcome::Active => {
                self.state
                    .lock()
                    .pending_address_space_reclaims
                    .push(address_space);
                match task_runtime::arm_address_space_reclaim(handle) {
                    AddressSpaceReclaimArmOutcome::Ready => self.task_work.publish(),
                    AddressSpaceReclaimArmOutcome::Armed => {}
                }
                return false;
            }
        }
        true
    }

    fn drain_deferred_coroutine_reclaims_inner(&self, limit: usize) -> Result<usize, TaskError> {
        const MAX_DRAIN_BATCH: usize = 64;

        let mut messages = [InboxMessage::EMPTY; MAX_DRAIN_BATCH];
        let batch = self
            .deferred_coroutine_reclaims
            .drain(limit.min(MAX_DRAIN_BATCH), &mut messages);
        for message in messages.iter().take(batch.drained()) {
            if message.operation() != InboxOperation::Reclaim || message.payload() == 0 {
                task_runtime::fatal_invariant(0x4558_0009, message.payload());
            }
            let header = ptr::with_exposed_provenance_mut::<CoroutineHeader>(message.payload());
            unsafe {
                // Detachment cleared the embedded reclaim membership. Zero
                // references and FUTURE_EMPTY make the type-erased allocation
                // exclusively owned by this task-context consumer.
                CoroutineHeader::deallocate_raw(header);
            }
        }
        Ok(batch.drained())
    }
}
