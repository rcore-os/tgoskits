const DELAYED_IDLE: u8 = 0;
const DELAYED_ARMED: u8 = 1;
const DELAYED_QUEUED: u8 = 2;
const DELAYED_PUBLISHING: u8 = 3;
const DELAYED_COMMAND_CANCEL: u64 = 0;
const DELAYED_COMMAND_IMMEDIATE: u64 = 1;
const DELAYED_DEADLINE_BIAS: u64 = 2;
const DELAYED_COMMAND_GENERATION_BIT: u64 = 1 << 63;
const DELAYED_COMMAND_PAYLOAD_MASK: u64 = !DELAYED_COMMAND_GENERATION_BIT;
const DELAYED_TIMER_OWNER_CLASS: u64 = 0x4158_5751_5449_4d52;

impl WorkQueue {
    /// Arms or moves one delayed activation on this queue's owner CPU.
    ///
    /// Remote and hard-IRQ callers only update atomics and queue the embedded
    /// control item. The target CPU's high-priority worker is the sole context
    /// that mutates its ax-task timer heap.
    pub fn mod_delayed_work_on(
        self: Pin<&'static Self>,
        cpu: usize,
        delayed: Pin<&'static DelayedWork>,
        delay_ns: u64,
    ) -> Result<ModDelayedWorkResult, WorkQueueError> {
        if cpu != self.cpu {
            return Err(WorkQueueError::InvalidCpu {
                cpu,
                cpu_count: crate::CPU_CAPACITY,
            });
        }
        if self.state() != WorkQueueState::Accepting {
            return Err(WorkQueueError::DomainNotAccepting);
        }
        delayed.bind(self)?;
        runtime_worker_wake(WorkerRoute::new(cpu, self.priority, crate::CPU_CAPACITY)?)?;
        runtime_worker_wake(WorkerRoute::new(
            cpu,
            WorkPriority::High,
            crate::CPU_CAPACITY,
        )?)?;

        let mut admission = DelayedDomainAdmission::acquire(self.get_ref())?;
        let command = if delay_ns == 0 {
            DELAYED_COMMAND_IMMEDIATE
        } else {
            ax_hal::time::monotonic_time_nanos()
                .checked_add(delay_ns)
                .and_then(|deadline| deadline.checked_add(DELAYED_DEADLINE_BIAS))
                .filter(|command| *command <= DELAYED_COMMAND_PAYLOAD_MASK)
                .ok_or(WorkQueueError::DelayOverflow)?
        };
        let modified = delayed.prepare_arm()?;
        if !modified {
            admission.transfer_to_delayed_work();
        }
        if let Err(error) = delayed.publish_command(command) {
            delayed.rollback_new_arm(self.get_ref(), modified);
            return Err(error);
        }
        if let Err(error) = queue_runtime_work(
            WorkerRoute {
                cpu,
                priority: WorkPriority::High,
            },
            delayed.control_work(),
        ) {
            delayed.rollback_new_arm(self.get_ref(), modified);
            return Err(error);
        }
        Ok(if modified {
            ModDelayedWorkResult::Modified
        } else {
            ModDelayedWorkResult::Scheduled
        })
    }

    /// Cancels a delayed timer and any already-queued activation, then waits
    /// for both fixed work items to become idle.
    pub fn cancel_delayed_work_sync(
        self: Pin<&'static Self>,
        delayed: Pin<&'static DelayedWork>,
    ) -> Result<bool, WorkQueueError> {
        if delayed.is_never_submitted() {
            return Ok(false);
        }
        delayed.ensure_queue(self.get_ref())?;
        ensure_runtime_wait_context(delayed.control_work().get_ref())?;
        let was_active =
            delayed.phase.load(Ordering::Acquire) != DELAYED_IDLE || !delayed.work().is_idle();
        self.wait_for_cancel_publication(delayed)?;
        self.cancel_work_sync(delayed.work())?;
        delayed.finish_queued_idle();
        Ok(was_active)
    }

    fn wait_for_cancel_publication(
        self: Pin<&'static Self>,
        delayed: Pin<&'static DelayedWork>,
    ) -> Result<(), WorkQueueError> {
        loop {
            delayed.publish_command(DELAYED_COMMAND_CANCEL)?;
            let _queue_result = queue_runtime_work(
                WorkerRoute {
                    cpu: self.cpu,
                    priority: WorkPriority::High,
                },
                delayed.control_work(),
            )?;
            let control = RUNTIME_WORKQUEUE.begin_flush(delayed.control_work());
            wait_for_runtime_completion(
                delayed.control_work().get_ref(),
                RuntimeCompletion::Flush(control),
            )?;
            match delayed.phase.load(Ordering::Acquire) {
                DELAYED_IDLE | DELAYED_QUEUED => return Ok(()),
                // An expiry owns the publication baton. Re-running the
                // control item after it leaves PUBLISHING either cancels the
                // restored timer generation or observes the queued work that
                // cancel_work_sync must consume.
                DELAYED_ARMED | DELAYED_PUBLISHING => {}
                _ => return Err(WorkQueueError::InvalidState),
            }
        }
    }

    /// Forces an armed delay to run now and waits for the accepted activation.
    pub fn flush_delayed_work(
        self: Pin<&'static Self>,
        delayed: Pin<&'static DelayedWork>,
    ) -> Result<(), WorkQueueError> {
        if delayed.is_never_submitted() {
            return Ok(());
        }
        delayed.ensure_queue(self.get_ref())?;
        ensure_runtime_wait_context(delayed.control_work().get_ref())?;
        self.wait_for_flush_publication(delayed)?;
        self.flush_work(delayed.work())?;
        delayed.finish_queued_idle();
        Ok(())
    }

    fn wait_for_flush_publication(
        self: Pin<&'static Self>,
        delayed: Pin<&'static DelayedWork>,
    ) -> Result<(), WorkQueueError> {
        loop {
            match delayed.phase.load(Ordering::Acquire) {
                DELAYED_IDLE | DELAYED_QUEUED => return Ok(()),
                DELAYED_ARMED | DELAYED_PUBLISHING => {}
                _ => return Err(WorkQueueError::InvalidState),
            }
            delayed.publish_command(DELAYED_COMMAND_IMMEDIATE)?;
            let _queue_result = queue_runtime_work(
                WorkerRoute {
                    cpu: self.cpu,
                    priority: WorkPriority::High,
                },
                delayed.control_work(),
            )?;
            let control = RUNTIME_WORKQUEUE.begin_flush(delayed.control_work());
            wait_for_runtime_completion(
                delayed.control_work().get_ref(),
                RuntimeCompletion::Flush(control),
            )?;
        }
    }
}

/// Result of publishing one delayed-work deadline.
#[cfg(feature = "workqueue")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModDelayedWorkResult {
    /// An idle delayed item acquired a new queue reservation.
    Scheduled,
    /// An already-armed timer was moved to the new deadline.
    Modified,
}

/// Asynchronous timer-control failure retained by a delayed work item.
#[cfg(feature = "workqueue")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DelayedWorkFailure {
    /// No owner-CPU timer slot was available.
    TimerCapacity = 1,
    /// The task timer facade rejected the owner-CPU operation.
    Runtime       = 2,
    /// A supposedly reserved work activation violated its intrusive state.
    WorkState     = 3,
}

struct DelayedDomainAdmission {
    queue: &'static WorkQueue,
    release_on_drop: bool,
}

impl DelayedDomainAdmission {
    fn acquire(queue: &'static WorkQueue) -> Result<Self, WorkQueueError> {
        queue.reserve_item()?;
        Ok(Self {
            queue,
            release_on_drop: true,
        })
    }

    fn transfer_to_delayed_work(&mut self) {
        self.release_on_drop = false;
    }
}

impl Drop for DelayedDomainAdmission {
    fn drop(&mut self) {
        if self.release_on_drop {
            self.queue.release_item_reservation();
        }
    }
}

/// One pinned delayed activation backed by an ax-task timer and two fixed work nodes.
///
/// `control_work` always runs on the target CPU's high-priority shared worker;
/// `work` runs on the caller-selected logical queue. The object must have
/// shutdown lifetime because a timer expiration already copied into CPU-local
/// safe-point storage may still carry its opaque address after cancellation.
#[cfg(feature = "workqueue")]
pub struct DelayedWork {
    work: WorkItem,
    control_work: WorkItem,
    timer: TimerNode,
    callback: fn(usize) -> WorkOutcome,
    callback_data: usize,
    owner_address: AtomicUsize,
    owner_queue: AtomicPtr<WorkQueue>,
    phase: AtomicU8,
    desired_command: AtomicU64,
    armed_command: AtomicU64,
    armed_token: AtomicU64,
    last_failure: AtomicU8,
    _pin: PhantomPinned,
}

#[cfg(feature = "workqueue")]
impl core::fmt::Debug for DelayedWork {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DelayedWork")
            .field("phase", &self.phase.load(Ordering::Acquire))
            .field(
                "desired_command",
                &self.desired_command.load(Ordering::Acquire),
            )
            .field("armed_command", &self.armed_command.load(Ordering::Acquire))
            .field("armed_token", &self.armed_token.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "workqueue")]
impl DelayedWork {
    /// Creates an idle delayed item suitable for static initialization.
    pub const fn new(callback: fn(usize) -> WorkOutcome, callback_data: usize) -> Self {
        Self {
            work: WorkItem::new(delayed_work_entry, 0),
            control_work: WorkItem::new(delayed_control_entry, 0),
            timer: TimerNode::new(0),
            callback,
            callback_data,
            owner_address: AtomicUsize::new(0),
            owner_queue: AtomicPtr::new(ptr::null_mut()),
            phase: AtomicU8::new(DELAYED_IDLE),
            desired_command: AtomicU64::new(DELAYED_COMMAND_CANCEL),
            armed_command: AtomicU64::new(0),
            armed_token: AtomicU64::new(0),
            last_failure: AtomicU8::new(0),
            _pin: PhantomPinned,
        }
    }

    /// Returns the last asynchronous control failure and clears it.
    pub fn take_failure(&self) -> Option<DelayedWorkFailure> {
        match self.last_failure.swap(0, Ordering::AcqRel) {
            0 => None,
            1 => Some(DelayedWorkFailure::TimerCapacity),
            2 => Some(DelayedWorkFailure::Runtime),
            3 => Some(DelayedWorkFailure::WorkState),
            _ => unreachable!("delayed work published an invalid failure code"),
        }
    }

    fn bind(
        self: Pin<&'static Self>,
        queue: Pin<&'static WorkQueue>,
    ) -> Result<(), WorkQueueError> {
        let delayed = self.get_ref();
        let address = ptr::from_ref(delayed).expose_provenance();
        match delayed.owner_address.compare_exchange(
            0,
            address,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(owner) if owner == address => {}
            Err(_) => return Err(WorkQueueError::InvalidState),
        }
        let queue_ptr = ptr::from_ref(queue.get_ref()).cast_mut();
        match delayed.owner_queue.compare_exchange(
            ptr::null_mut(),
            queue_ptr,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {}
            Err(owner) if owner == queue_ptr => {}
            Err(_) => return Err(WorkQueueError::ForeignDomain),
        }
        delayed.work.bind_private_callback_data(address)?;
        delayed.control_work.bind_private_callback_data(address)?;
        delayed.work.bind_domain(queue.get_ref())?;
        delayed.work.bind_system(&RUNTIME_WORKQUEUE)?;
        delayed.control_work.bind_system(&RUNTIME_WORKQUEUE)?;
        Ok(())
    }

    fn ensure_queue(&self, queue: &WorkQueue) -> Result<(), WorkQueueError> {
        if self.owner_queue.load(Ordering::Acquire) == ptr::from_ref(queue).cast_mut() {
            Ok(())
        } else {
            Err(WorkQueueError::ForeignDomain)
        }
    }

    fn prepare_arm(&self) -> Result<bool, WorkQueueError> {
        loop {
            match self.phase.load(Ordering::Acquire) {
                DELAYED_IDLE => {
                    if !self.work.is_idle() {
                        return Err(WorkQueueError::DelayedWorkBusy);
                    }
                    if self
                        .phase
                        .compare_exchange(
                            DELAYED_IDLE,
                            DELAYED_ARMED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Ok(false);
                    }
                }
                DELAYED_ARMED => return Ok(true),
                DELAYED_QUEUED if self.work.is_idle() => {
                    let _ = self.phase.compare_exchange(
                        DELAYED_QUEUED,
                        DELAYED_IDLE,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                }
                DELAYED_QUEUED | DELAYED_PUBLISHING => {
                    return Err(WorkQueueError::DelayedWorkBusy);
                }
                _ => return Err(WorkQueueError::InvalidState),
            }
        }
    }

    fn publish_command(&self, command: u64) -> Result<u64, WorkQueueError> {
        if command > DELAYED_COMMAND_PAYLOAD_MASK {
            return Err(WorkQueueError::DelayOverflow);
        }
        // The CAS is the producer linearization point: a worker can never
        // observe one producer's generation paired with another producer's
        // deadline. The toggle also distinguishes an identical republish;
        // additional updates may coalesce only when their final payload is
        // equivalent, while WorkItem::RERUN preserves worker activation.
        let mut observed = self.desired_command.load(Ordering::Acquire);
        loop {
            let generation =
                (observed ^ DELAYED_COMMAND_GENERATION_BIT) & DELAYED_COMMAND_GENERATION_BIT;
            let published = generation | command;
            match self.desired_command.compare_exchange_weak(
                observed,
                published,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(published),
                Err(current) => observed = current,
            }
        }
    }

    fn work(self: Pin<&'static Self>) -> Pin<&'static WorkItem> {
        unsafe {
            // SAFETY: DelayedWork is pinned for shutdown lifetime, so its
            // embedded intrusive WorkItem cannot move.
            Pin::new_unchecked(&self.get_ref().work)
        }
    }

    fn control_work(self: Pin<&'static Self>) -> Pin<&'static WorkItem> {
        unsafe {
            // SAFETY: identical stable-address projection to `work`.
            Pin::new_unchecked(&self.get_ref().control_work)
        }
    }

    fn is_never_submitted(&self) -> bool {
        self.owner_address.load(Ordering::Acquire) == 0
            && self.phase.load(Ordering::Acquire) == DELAYED_IDLE
            && self.work.is_idle()
            && self.control_work.is_idle()
    }

    fn finish_queued_idle(&self) {
        if self.work.is_idle() {
            let _ = self.phase.compare_exchange(
                DELAYED_QUEUED,
                DELAYED_IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    fn rollback_new_arm(&self, queue: &'static WorkQueue, modified: bool) {
        if !modified
            && self
                .phase
                .compare_exchange(
                    DELAYED_ARMED,
                    DELAYED_IDLE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
        {
            queue.release_item_reservation();
        }
    }

    fn owner_queue(&self) -> Result<&'static WorkQueue, WorkQueueError> {
        let queue = self.owner_queue.load(Ordering::Acquire);
        if queue.is_null() {
            return Err(WorkQueueError::ForeignDomain);
        }
        Ok(unsafe {
            // SAFETY: binding accepts only a pinned shutdown-lifetime queue and
            // the pointer is never replaced afterwards.
            &*queue
        })
    }

    fn publish_failure_activation(&'static self, failure: DelayedWorkFailure) {
        self.last_failure.store(failure as u8, Ordering::Release);
        if self
            .phase
            .compare_exchange(
                DELAYED_ARMED,
                DELAYED_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        let Ok(queue) = self.owner_queue() else {
            self.phase.store(DELAYED_IDLE, Ordering::Release);
            return;
        };
        let route = WorkerRoute {
            cpu: queue.cpu,
            priority: queue.priority,
        };
        let work = unsafe {
            // SAFETY: delayed work is pinned for shutdown lifetime after bind.
            Pin::new_unchecked(&self.work)
        };
        if queue_runtime_work(route, work).is_ok() {
            self.phase.store(DELAYED_QUEUED, Ordering::Release);
        } else {
            self.phase.store(DELAYED_IDLE, Ordering::Release);
            queue.release_item_reservation();
        }
    }
}

#[cfg(feature = "workqueue")]
fn runtime_worker_wake(route: WorkerRoute) -> Result<&'static ThreadWakeHandle, WorkQueueError> {
    RUNTIME_WORKQUEUE.lane(route)?.worker_wake_handle()
}

#[cfg(feature = "workqueue")]
fn queue_runtime_work(
    route: WorkerRoute,
    work: Pin<&'static WorkItem>,
) -> Result<QueueWorkResult, WorkQueueError> {
    let lane = RUNTIME_WORKQUEUE.lane(route)?;
    let wake = lane.worker_wake_handle()?;
    let result = RUNTIME_WORKQUEUE.queue_work_on(route.cpu, route.priority, work)?;
    match result {
        QueueWorkResult::Queued => {
            let wake_result = wake.wake();
            enforce_published_worker_progress(lane, wake_result);
            Ok(result)
        }
        QueueWorkResult::AlreadyPending | QueueWorkResult::RerunRequested => Ok(result),
        QueueWorkResult::CancelInProgress => Err(WorkQueueError::InvalidState),
    }
}

#[cfg(feature = "workqueue")]
fn delayed_control_entry(data: usize) -> WorkOutcome {
    let delayed = unsafe {
        // SAFETY: `bind` publishes only the address of a pinned,
        // shutdown-lifetime DelayedWork before this control item can run.
        &*ptr::with_exposed_provenance::<DelayedWork>(data)
    };
    delayed.service_control()
}

#[cfg(feature = "workqueue")]
fn delayed_work_entry(data: usize) -> WorkOutcome {
    let delayed = unsafe {
        // SAFETY: identical pinned owner publication to `delayed_control_entry`.
        &*ptr::with_exposed_provenance::<DelayedWork>(data)
    };
    (delayed.callback)(delayed.callback_data)
}

#[cfg(feature = "workqueue")]
impl DelayedWork {
    fn service_control(&'static self) -> WorkOutcome {
        let command = self.desired_command.load(Ordering::Acquire);
        let command_payload = command & DELAYED_COMMAND_PAYLOAD_MASK;
        self.armed_command.store(0, Ordering::Release);
        if let Some(token) = TimerToken::from_generation(self.armed_token.swap(0, Ordering::AcqRel))
        {
            let timer = unsafe {
                // SAFETY: `self` has shutdown lifetime after callback-data binding.
                Pin::new_unchecked(&self.timer)
            };
            if cancel_current_runtime_timer(timer, token).is_err() {
                self.publish_failure_activation(DelayedWorkFailure::Runtime);
                return WorkOutcome::Complete;
            }
        }

        if self.desired_command.load(Ordering::Acquire) != command {
            return WorkOutcome::Requeue;
        }
        if command_payload == DELAYED_COMMAND_CANCEL {
            if self
                .phase
                .compare_exchange(
                    DELAYED_ARMED,
                    DELAYED_IDLE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
                && let Ok(queue) = self.owner_queue()
            {
                queue.release_item_reservation();
            }
            return WorkOutcome::Complete;
        }
        if command_payload == DELAYED_COMMAND_IMMEDIATE {
            return match self.publish_expired_activation(command, 0) {
                Ok(()) => WorkOutcome::Complete,
                Err(()) => WorkOutcome::Requeue,
            };
        }

        let Some(deadline_ns) = command_payload.checked_sub(DELAYED_DEADLINE_BIAS) else {
            self.publish_failure_activation(DelayedWorkFailure::WorkState);
            return WorkOutcome::Complete;
        };
        let owner_address = self.owner_address.load(Ordering::Acquire);
        let owner = unsafe {
            // SAFETY: binding published this pinned shutdown-lifetime object,
            // and the non-zero class uniquely selects DelayedWork in runtime.
            RuntimeTimerOwner::new(owner_address, DELAYED_TIMER_OWNER_CLASS)
        };
        let timer = unsafe {
            // SAFETY: callback-data binding proves shutdown-lifetime pinning.
            Pin::new_unchecked(&self.timer)
        };
        let preempt = PreemptGuard::new();
        let token = match arm_current_runtime_timer(timer, deadline_ns, owner) {
            Ok(token) => token,
            Err(TaskError::TimerCapacity) => {
                drop(preempt);
                if self.desired_command.load(Ordering::Acquire) != command {
                    return WorkOutcome::Requeue;
                }
                self.publish_failure_activation(DelayedWorkFailure::TimerCapacity);
                return WorkOutcome::Complete;
            }
            Err(_) => {
                drop(preempt);
                if self.desired_command.load(Ordering::Acquire) != command {
                    return WorkOutcome::Requeue;
                }
                self.publish_failure_activation(DelayedWorkFailure::Runtime);
                return WorkOutcome::Complete;
            }
        };
        self.armed_token
            .store(token.generation(), Ordering::Release);
        self.armed_command.store(command, Ordering::Release);
        drop(preempt);
        if self.desired_command.load(Ordering::Acquire) == command {
            WorkOutcome::Complete
        } else {
            WorkOutcome::Requeue
        }
    }

    fn publish_expired_activation(&'static self, command: u64, token: u64) -> Result<(), ()> {
        if token != 0
            && (self.armed_command.load(Ordering::Acquire) != command
                || self.armed_token.load(Ordering::Acquire) != token)
        {
            return Ok(());
        }
        if self
            .phase
            .compare_exchange(
                DELAYED_ARMED,
                DELAYED_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Ok(());
        }
        if self.desired_command.load(Ordering::Acquire) != command {
            let _ = self.phase.compare_exchange(
                DELAYED_PUBLISHING,
                DELAYED_ARMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(());
        }
        if token != 0
            && self
                .armed_token
                .compare_exchange(token, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            let _ = self.phase.compare_exchange(
                DELAYED_PUBLISHING,
                DELAYED_ARMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Ok(());
        }
        self.armed_command.store(0, Ordering::Release);
        let queue = match self.owner_queue() {
            Ok(queue) => queue,
            Err(_) => {
                // An ARMED delayed item must already be bound to one
                // shutdown-lifetime queue. Do not leave a corrupt or forged
                // timer owner holding the publication baton forever: consume
                // this expiration as a failed activation and make every late
                // copy of the same timer event a stale no-op.
                self.last_failure
                    .store(DelayedWorkFailure::WorkState as u8, Ordering::Release);
                let restored = self.phase.compare_exchange(
                    DELAYED_PUBLISHING,
                    DELAYED_IDLE,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                debug_assert!(restored.is_ok(), "expiration publication lost its baton");
                return Err(());
            }
        };
        let route = WorkerRoute {
            cpu: queue.cpu,
            priority: queue.priority,
        };
        let work = unsafe {
            // SAFETY: `self` is shutdown-lived and its work node is pinned.
            Pin::new_unchecked(&self.work)
        };
        if queue_runtime_work(route, work).is_err() {
            self.last_failure
                .store(DelayedWorkFailure::WorkState as u8, Ordering::Release);
            self.phase.store(DELAYED_IDLE, Ordering::Release);
            queue.release_item_reservation();
            return Ok(());
        }
        self.phase.store(DELAYED_QUEUED, Ordering::Release);
        Ok(())
    }
}

/// Validates and queues one delayed-work expiration from TaskRuntime's safe-point hook.
#[cfg(feature = "workqueue")]
pub(crate) fn dispatch_expired_timer(event: RuntimeTimerEventV1) -> RuntimeStatus {
    // Unknown classes and malformed shutdown-lifetime owners are provider
    // invariants, not stale delayed-work generations. Returning a failure is
    // intentional: ax-task retains that record instead of dispatching later
    // expirations past an event whose ownership cannot be established.
    if event.owner_class != DELAYED_TIMER_OWNER_CLASS {
        return RuntimeStatus::Unsupported;
    }
    if event.owner == 0
        || !event
            .owner
            .is_multiple_of(core::mem::align_of::<DelayedWork>())
    {
        return RuntimeStatus::InvalidHandle;
    }
    let delayed: &'static DelayedWork = unsafe {
        // SAFETY: RuntimeTimerOwner creation requires this class/address pair
        // to name a pinned DelayedWork through safe-point delivery.
        &*ptr::with_exposed_provenance::<DelayedWork>(event.owner)
    };
    if delayed.owner_address.load(Ordering::Acquire) != event.owner
        || event.node != ptr::from_ref(&delayed.timer).expose_provenance()
    {
        return RuntimeStatus::InvalidHandle;
    }
    let command = delayed.armed_command.load(Ordering::Acquire);
    if (command & DELAYED_COMMAND_PAYLOAD_MASK) < DELAYED_DEADLINE_BIAS
        || command != delayed.desired_command.load(Ordering::Acquire)
        || event.token_generation != delayed.armed_token.load(Ordering::Acquire)
    {
        return RuntimeStatus::Success;
    }
    match delayed.publish_expired_activation(command, event.token_generation) {
        // Once owner identity is proven, every non-publication result is a
        // deliberate stale/superseded activation and must be consumed.
        Ok(()) => RuntimeStatus::Success,
        Err(()) => RuntimeStatus::Success,
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;

    fn complete_work(_data: usize) -> WorkOutcome {
        WorkOutcome::Complete
    }

    #[test]
    fn invalid_expiration_owner_releases_the_publication_baton() {
        let delayed = Box::leak(Box::new(DelayedWork::new(complete_work, 0)));
        let command = DELAYED_DEADLINE_BIAS + 100;
        delayed.desired_command.store(command, Ordering::Release);
        delayed.phase.store(DELAYED_ARMED, Ordering::Release);

        assert_eq!(delayed.publish_expired_activation(command, 0), Err(()));
        assert_eq!(
            delayed.phase.load(Ordering::Acquire),
            DELAYED_IDLE,
            "a rejected expiration must not retain the PUBLISHING baton"
        );
        assert_eq!(delayed.take_failure(), Some(DelayedWorkFailure::WorkState));
        assert_eq!(delayed.publish_expired_activation(command, 0), Ok(()));
        assert_eq!(delayed.phase.load(Ordering::Acquire), DELAYED_IDLE);
    }

    #[test]
    fn failed_expiration_publication_releases_one_domain_reservation() {
        let queue_ref: &'static WorkQueue =
            Box::leak(Box::new(WorkQueue::new(0, WorkPriority::High)));
        let queue = unsafe {
            // SAFETY: the test deliberately leaks this logical queue so every
            // intrusive pointer remains valid for the test process lifetime.
            Pin::new_unchecked(queue_ref)
        };
        let delayed_ref: &'static DelayedWork =
            Box::leak(Box::new(DelayedWork::new(complete_work, 0)));
        let delayed = unsafe {
            // SAFETY: the delayed item is also leaked and never moved again.
            Pin::new_unchecked(delayed_ref)
        };
        delayed.bind(queue).unwrap();
        queue.reserve_item().unwrap();
        // Deterministically force the runtime submission path to reject the
        // embedded work after the delayed item already owns one domain slot.
        delayed_ref.work.state.store(CANCELLING, Ordering::Release);
        let command = DELAYED_DEADLINE_BIAS + 100;
        delayed_ref
            .desired_command
            .store(command, Ordering::Release);
        delayed_ref.phase.store(DELAYED_ARMED, Ordering::Release);

        assert_eq!(delayed_ref.publish_expired_activation(command, 0), Ok(()));
        assert_eq!(delayed_ref.phase.load(Ordering::Acquire), DELAYED_IDLE);
        let drain = queue.begin_drain().unwrap();
        assert!(
            drain.is_complete(),
            "a failed publication must release its sole domain reservation"
        );
        assert_eq!(delayed_ref.publish_expired_activation(command, 0), Ok(()));
        assert!(
            drain.is_complete(),
            "a late expiration must not release twice"
        );
    }

    #[test]
    fn timer_dispatcher_distinguishes_stale_events_from_invalid_owners() {
        let queue_ref: &'static WorkQueue =
            Box::leak(Box::new(WorkQueue::new(0, WorkPriority::High)));
        let queue = unsafe {
            // SAFETY: both test objects are leaked for process lifetime.
            Pin::new_unchecked(queue_ref)
        };
        let delayed_ref: &'static DelayedWork =
            Box::leak(Box::new(DelayedWork::new(complete_work, 0)));
        let delayed = unsafe {
            // SAFETY: both test objects are leaked for process lifetime.
            Pin::new_unchecked(delayed_ref)
        };
        delayed.bind(queue).unwrap();

        let stale = RuntimeTimerEventV1 {
            owner: ptr::from_ref(delayed_ref).expose_provenance(),
            node: ptr::from_ref(&delayed_ref.timer).expose_provenance(),
            owner_class: DELAYED_TIMER_OWNER_CLASS,
            token_generation: 1,
            deadline_ns: 100,
        };
        assert_eq!(dispatch_expired_timer(stale), RuntimeStatus::Success);

        assert_eq!(
            dispatch_expired_timer(RuntimeTimerEventV1 { owner: 0, ..stale }),
            RuntimeStatus::InvalidHandle
        );
        assert_eq!(
            dispatch_expired_timer(RuntimeTimerEventV1 {
                owner_class: DELAYED_TIMER_OWNER_CLASS.wrapping_add(1),
                ..stale
            }),
            RuntimeStatus::Unsupported
        );
    }
}
