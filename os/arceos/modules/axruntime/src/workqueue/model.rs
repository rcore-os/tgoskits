/// Snapshot of activations accepted before a flush observation.
///
/// This is intentionally non-blocking. A task-context adapter may park while
/// polling the ticket, but IRQ and callback contexts can only inspect it.
#[derive(Clone, Copy, Debug)]
#[must_use]
pub struct FlushToken {
    work: &'static WorkItem,
    target_epoch: u64,
}

/// Bounded callback decision consumed by the owning shared worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum WorkOutcome {
    /// This activation finished and needs no worker-owned continuation.
    Complete,
    /// Queue exactly one later activation after this callback returns.
    Requeue,
}

impl FlushToken {
    /// Returns whether every activation accepted before this snapshot ended.
    pub fn is_complete(self) -> bool {
        self.work.completion_reached(self.target_epoch)
    }
}

/// Cancellation snapshot tied to the accepted work epoch it suppresses.
#[derive(Clone, Copy, Debug)]
#[must_use]
pub struct CancelToken {
    work: &'static WorkItem,
    target_epoch: u64,
    was_pending: bool,
}

impl CancelToken {
    /// Returns whether queue or callback state existed when cancellation began.
    pub const fn was_pending(self) -> bool {
        self.was_pending
    }

    /// Returns whether the worker consumed the tombstone or callback exited.
    pub fn is_complete(self) -> bool {
        self.work.completion_reached(self.target_epoch)
    }
}

/// Result of one bounded shared-worker service pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub struct WorkBatch {
    examined: usize,
    executed: usize,
    cancelled: usize,
    pending: bool,
    limit: usize,
}

impl WorkBatch {
    /// Number of callbacks invoked by this pass.
    pub const fn executed(self) -> usize {
        self.executed
    }

    /// Number of queued tombstones consumed without invoking their callback.
    pub const fn cancelled(self) -> usize {
        self.cancelled
    }

    /// Returns whether this logical lane still needs another pass.
    pub const fn pending(self) -> bool {
        self.pending
    }

    /// Returns whether the pass reached its node budget.
    pub const fn saturated(self) -> bool {
        self.examined == self.limit
    }
}

/// Two fixed shared-worker lanes for every represented logical CPU.
///
/// This object is the queue topology, not a user-created workqueue. A runtime
/// installs at most one normal and one high-priority consumer for each CPU and
/// calls [`Self::service_batch`] from those fixed workers.
#[derive(Debug)]
pub struct WorkQueueSystem<const CPU_COUNT: usize> {
    normal: [WorkerLane; CPU_COUNT],
    high: [WorkerLane; CPU_COUNT],
}

impl<const CPU_COUNT: usize> WorkQueueSystem<CPU_COUNT> {
    /// Creates an empty fixed topology without allocating.
    pub const fn new() -> Self {
        Self {
            normal: [const { WorkerLane::new() }; CPU_COUNT],
            high: [const { WorkerLane::new() }; CPU_COUNT],
        }
    }

    /// Queues one activation on a CPU's shared logical worker lane.
    ///
    /// The hard-IRQ path is limited to atomic state publication and one
    /// intrusive MPSC push. It neither invokes `callback` nor allocates. A
    /// repeated request coalesces while queued; one request received while the
    /// callback is running becomes a single `RERUN` activation. An active item
    /// retains its existing CPU and priority route; the requested route is
    /// applied only when the item was idle. The system itself has shutdown
    /// lifetime because each item permanently records its owning topology
    /// identity.
    pub fn queue_work_on(
        &'static self,
        cpu: usize,
        priority: WorkPriority,
        work: Pin<&'static WorkItem>,
    ) -> Result<QueueWorkResult, WorkQueueError> {
        let route = WorkerRoute::new(cpu, priority, CPU_COUNT)?;
        let lane = self.lane(route)?;
        let work = work.get_ref();
        work.bind_system(self)?;

        loop {
            let observed = work.state.load(Ordering::Acquire);
            let flags = state_flags(observed);
            if flags & CANCELLING != 0 {
                return Err(WorkQueueError::CancelInProgress);
            }

            let (updated, result, publish) = match flags {
                0 => (
                    queued_state(next_epoch(observed)?, route),
                    QueueWorkResult::Queued,
                    true,
                ),
                QUEUED => return Ok(QueueWorkResult::AlreadyPending),
                RUNNING => {
                    // A producer may target this item through another CPU or
                    // priority lane while its callback is running. The
                    // current worker still owns the item until callback exit,
                    // so the coalesced rerun must stay on that owner route.
                    // Rebinding here would silently migrate a fixed-affinity
                    // work item and bypass its logical WorkQueue domain.
                    let owner = state_route(observed).ok_or(WorkQueueError::InvalidState)?;
                    (
                        running_rerun_state(next_epoch(observed)?, owner),
                        QueueWorkResult::RerunRequested,
                        false,
                    )
                }
                flags if flags == RUNNING | RERUN => {
                    return Ok(QueueWorkResult::AlreadyPending);
                }
                _ => return Err(WorkQueueError::InvalidState),
            };

            if work
                .state
                .compare_exchange_weak(observed, updated, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                if publish {
                    lane.publish(work);
                }
                return Ok(result);
            }
        }
    }

    /// Queues one activation through a logical admission and drain domain.
    ///
    /// This is the scheduler-independent domain model used by host tests and
    /// by the runtime facade after it has established a worker progress
    /// guarantee. A provisional reservation closes the race with
    /// [`WorkQueue::begin_drain`]; only the first idle-to-queued activation
    /// retains that reservation because an already-active item already owns
    /// one domain slot.
    #[cfg(feature = "workqueue")]
    pub fn queue_work_in_domain(
        &'static self,
        domain: Pin<&'static WorkQueue>,
        work: Pin<&'static WorkItem>,
    ) -> Result<QueueWorkResult, WorkQueueError> {
        let domain = domain.get_ref();
        domain.reserve_item()?;
        if let Err(error) = work.get_ref().bind_domain(domain) {
            domain.release_item_reservation();
            return Err(error);
        }
        let result = match self.queue_work_on(domain.cpu, domain.priority, work) {
            Ok(result) => result,
            Err(error) => {
                domain.release_item_reservation();
                return Err(error);
            }
        };
        if result != QueueWorkResult::Queued {
            domain.release_item_reservation();
        }
        Ok(result)
    }

    /// Marks queued or running work for cancellation without blocking.
    ///
    /// Queued work remains an intrusive tombstone until its lane worker
    /// consumes it; allowing immediate reuse would let one node appear twice in
    /// the MPSC list. Running work finishes its current callback but any
    /// accepted rerun is suppressed.
    pub fn begin_cancel(&self, work: Pin<&'static WorkItem>) -> CancelToken {
        let work = work.get_ref();
        loop {
            let observed = work.state.load(Ordering::Acquire);
            let flags = state_flags(observed);
            let target_epoch = state_epoch(observed);
            if flags == 0 {
                return CancelToken {
                    work,
                    target_epoch,
                    was_pending: false,
                };
            }
            if flags & CANCELLING != 0 {
                return CancelToken {
                    work,
                    target_epoch,
                    was_pending: true,
                };
            }
            debug_assert!(flags & (QUEUED | RUNNING) != 0);
            if work
                .state
                .compare_exchange_weak(
                    observed,
                    observed | CANCELLING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return CancelToken {
                    work,
                    target_epoch,
                    was_pending: true,
                };
            }
        }
    }

    /// Snapshots the latest accepted activation without blocking.
    pub fn begin_flush(&self, work: Pin<&'static WorkItem>) -> FlushToken {
        work.get_ref().flush_token()
    }

    /// Reports whether a CPU's logical lane needs service.
    pub fn has_pending(&self, cpu: usize, priority: WorkPriority) -> Result<bool, WorkQueueError> {
        let route = WorkerRoute::new(cpu, priority, CPU_COUNT)?;
        Ok(self.lane(route)?.has_pending())
    }

    /// Services at most [`WORK_BATCH_LIMIT`] intrusive nodes from one lane.
    pub fn service_batch(
        &self,
        cpu: usize,
        priority: WorkPriority,
    ) -> Result<WorkBatch, WorkQueueError> {
        self.service_batch_with_limit(cpu, priority, WORK_BATCH_LIMIT)
    }

    /// Services a caller-selected number of nodes up to [`WORK_BATCH_LIMIT`].
    ///
    /// This is the UP host fake-executor boundary used by tests and the future
    /// fixed worker adapter. Cancellation tombstones consume the same budget as
    /// callbacks, so an interrupt storm cannot create an unbounded pass.
    pub fn service_batch_with_limit(
        &self,
        cpu: usize,
        priority: WorkPriority,
        limit: usize,
    ) -> Result<WorkBatch, WorkQueueError> {
        self.service_batch_with_callback(cpu, priority, limit, |work| {
            (work.callback)(work.callback_data.load(Ordering::Acquire))
        })
    }

    fn service_batch_with_callback(
        &self,
        cpu: usize,
        priority: WorkPriority,
        limit: usize,
        mut invoke: impl FnMut(&'static WorkItem) -> WorkOutcome,
    ) -> Result<WorkBatch, WorkQueueError> {
        if limit == 0 {
            return Err(WorkQueueError::EmptyBatch);
        }
        if limit > WORK_BATCH_LIMIT {
            return Err(WorkQueueError::BatchLimitExceeded {
                requested: limit,
                maximum: WORK_BATCH_LIMIT,
            });
        }
        let route = WorkerRoute::new(cpu, priority, CPU_COUNT)?;
        let lane = self.lane(route)?;
        let mut consumer = lane.try_claim_consumer()?;
        lane.consume_doorbell();

        let mut examined = 0;
        let mut executed = 0;
        let mut cancelled = 0;
        while examined < limit {
            let Some(work) = consumer.pop() else {
                break;
            };
            examined += 1;
            match self.service_one(route, work, &mut invoke)? {
                ServiceResult::Executed => executed += 1,
                ServiceResult::Cancelled => cancelled += 1,
            }
        }

        let pending = lane.structural_pending();
        if pending {
            lane.reassert_doorbell();
        }
        Ok(WorkBatch {
            examined,
            executed,
            cancelled,
            pending,
            limit,
        })
    }

    #[cfg(feature = "workqueue")]
    fn service_runtime_batch(
        &self,
        cpu: usize,
        priority: WorkPriority,
    ) -> Result<WorkBatch, WorkQueueError> {
        self.service_batch_with_callback(cpu, priority, WORK_BATCH_LIMIT, |work| {
            // Runtime callbacks are deliberately non-sleeping. Keeping this
            // guard scoped to one callback lets the scheduler preempt between
            // items while every normal block/yield entry made by the callback
            // observes an explicit unsafe-context error.
            let _preempt = PreemptGuard::new();
            (work.callback)(work.callback_data.load(Ordering::Acquire))
        })
    }

    fn service_one(
        &self,
        owner: WorkerRoute,
        work: &'static WorkItem,
        invoke: &mut impl FnMut(&'static WorkItem) -> WorkOutcome,
    ) -> Result<ServiceResult, WorkQueueError> {
        loop {
            let observed = work.state.load(Ordering::Acquire);
            let flags = state_flags(observed);
            if state_route(observed) != Some(owner) {
                return Err(WorkQueueError::InvalidState);
            }
            if flags == QUEUED | CANCELLING {
                let idle = idle_state(state_epoch(observed));
                if work
                    .state
                    .compare_exchange_weak(observed, idle, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    work.mark_completed_through(state_epoch(observed));
                    work.release_domain_item();
                    #[cfg(feature = "workqueue")]
                    if self.lane(owner)?.worker_id() != 0 {
                        work.publish_completion();
                    }
                    return Ok(ServiceResult::Cancelled);
                }
                continue;
            }
            if flags != QUEUED {
                return Err(WorkQueueError::InvalidState);
            }
            let running = running_state(state_epoch(observed), owner);
            if work
                .state
                .compare_exchange_weak(observed, running, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }

        #[cfg(feature = "workqueue")]
        let worker_id = self.lane(owner)?.worker_id();
        #[cfg(feature = "workqueue")]
        work.executing_worker.store(worker_id, Ordering::Release);
        let outcome = invoke(work);
        let finish = self.finish_callback(work, outcome);
        #[cfg(feature = "workqueue")]
        {
            work.executing_worker.store(0, Ordering::Release);
            if worker_id != 0 && finish.is_ok() {
                work.publish_completion();
            }
        }
        finish?;
        Ok(ServiceResult::Executed)
    }

    fn finish_callback(
        &self,
        work: &'static WorkItem,
        outcome: WorkOutcome,
    ) -> Result<(), WorkQueueError> {
        loop {
            let observed = work.state.load(Ordering::Acquire);
            let flags = state_flags(observed);
            let epoch = state_epoch(observed);
            if flags & RUNNING == 0 {
                return Err(WorkQueueError::InvalidState);
            }

            if flags & CANCELLING != 0 {
                if work
                    .state
                    .compare_exchange_weak(
                        observed,
                        idle_state(epoch),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    work.mark_completed_through(epoch);
                    work.release_domain_item();
                    return Ok(());
                }
                continue;
            }

            if flags == RUNNING | RERUN {
                let Some(route) = state_route(observed) else {
                    return Err(WorkQueueError::InvalidState);
                };
                if work
                    .state
                    .compare_exchange_weak(
                        observed,
                        queued_state(epoch, route),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    work.mark_one_completed();
                    self.lane(route)?.publish(work);
                    return Ok(());
                }
                continue;
            }

            if flags != RUNNING {
                return Err(WorkQueueError::InvalidState);
            }

            if outcome == WorkOutcome::Requeue && work.domain_allows_callback_continuation() {
                let Some(route) = state_route(observed) else {
                    return Err(WorkQueueError::InvalidState);
                };
                let next_epoch = match next_epoch(observed) {
                    Ok(next_epoch) => next_epoch,
                    Err(error) => {
                        if work
                            .state
                            .compare_exchange_weak(
                                observed,
                                idle_state(epoch),
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            work.mark_one_completed();
                            work.release_domain_item();
                            return Err(error);
                        }
                        continue;
                    }
                };
                if work
                    .state
                    .compare_exchange_weak(
                        observed,
                        queued_state(next_epoch, route),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    work.mark_one_completed();
                    self.lane(route)?.publish(work);
                    return Ok(());
                }
                continue;
            }

            if work
                .state
                .compare_exchange_weak(
                    observed,
                    idle_state(epoch),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                work.mark_one_completed();
                work.release_domain_item();
                return Ok(());
            }
        }
    }

    fn lane(&self, route: WorkerRoute) -> Result<&WorkerLane, WorkQueueError> {
        let lanes = match route.priority {
            WorkPriority::Normal => &self.normal,
            WorkPriority::High => &self.high,
        };
        lanes.get(route.cpu).ok_or(WorkQueueError::InvalidCpu {
            cpu: route.cpu,
            cpu_count: CPU_COUNT,
        })
    }

    #[cfg(feature = "workqueue")]
    fn owns_worker_thread(&self, thread: u64) -> bool {
        thread != 0
            && self
                .normal
                .iter()
                .chain(self.high.iter())
                .any(|lane| lane.worker_id() == thread)
    }
}

impl<const CPU_COUNT: usize> Default for WorkQueueSystem<CPU_COUNT> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkerRoute {
    cpu: usize,
    priority: WorkPriority,
}

impl WorkerRoute {
    fn new(cpu: usize, priority: WorkPriority, cpu_count: usize) -> Result<Self, WorkQueueError> {
        if cpu >= cpu_count {
            return Err(WorkQueueError::InvalidCpu { cpu, cpu_count });
        }
        let encoded = cpu
            .checked_mul(2)
            .and_then(|value| value.checked_add(priority as usize + 1))
            .ok_or(WorkQueueError::InvalidCpu { cpu, cpu_count })?;
        if encoded as u64 > ROUTE_VALUE_MASK {
            return Err(WorkQueueError::InvalidCpu { cpu, cpu_count });
        }
        Ok(Self { cpu, priority })
    }

    fn encode(self) -> u64 {
        (self.cpu * 2 + self.priority as usize + 1) as u64
    }

    fn decode(encoded: u64) -> Option<Self> {
        let index = encoded.checked_sub(1)? as usize;
        Some(Self {
            cpu: index / 2,
            priority: if index & 1 == 0 {
                WorkPriority::Normal
            } else {
                WorkPriority::High
            },
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServiceResult {
    Executed,
    Cancelled,
}

#[derive(Debug)]
struct WorkerLane {
    incoming: AtomicPtr<WorkItem>,
    consumer_active: AtomicBool,
    consumer_backlog: UnsafeCell<*mut WorkItem>,
    backlog_pending: AtomicBool,
    doorbell: AtomicBool,
    #[cfg(feature = "workqueue")]
    worker_state: AtomicU8,
    #[cfg(feature = "workqueue")]
    worker_thread: AtomicU64,
    #[cfg(feature = "workqueue")]
    worker_wake: AtomicPtr<ThreadWakeHandle>,
    #[cfg(feature = "workqueue")]
    worker_poisoned: AtomicBool,
    #[cfg(feature = "workqueue")]
    worker_park: WaitQueue,
}

impl WorkerLane {
    const fn new() -> Self {
        Self {
            incoming: AtomicPtr::new(ptr::null_mut()),
            consumer_active: AtomicBool::new(false),
            consumer_backlog: UnsafeCell::new(ptr::null_mut()),
            backlog_pending: AtomicBool::new(false),
            doorbell: AtomicBool::new(false),
            #[cfg(feature = "workqueue")]
            worker_state: AtomicU8::new(WORKER_UNINITIALIZED),
            #[cfg(feature = "workqueue")]
            worker_thread: AtomicU64::new(0),
            #[cfg(feature = "workqueue")]
            worker_wake: AtomicPtr::new(ptr::null_mut()),
            #[cfg(feature = "workqueue")]
            worker_poisoned: AtomicBool::new(false),
            #[cfg(feature = "workqueue")]
            worker_park: WaitQueue::new(),
        }
    }

    fn publish(&self, work: &'static WorkItem) {
        debug_assert!(work.next.load(Ordering::Relaxed).is_null());
        let work_ptr = ptr::from_ref(work).cast_mut();
        let mut head = self.incoming.load(Ordering::SeqCst);
        loop {
            work.next.store(head, Ordering::Relaxed);
            match self.incoming.compare_exchange_weak(
                head,
                work_ptr,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(observed) => head = observed,
            }
        }
        self.doorbell.store(true, Ordering::SeqCst);
    }

    fn try_claim_consumer(&self) -> Result<LaneConsumer<'_>, WorkQueueError> {
        self.consumer_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| WorkQueueError::WorkerBusy)?;
        Ok(LaneConsumer { lane: self })
    }

    fn consume_doorbell(&self) {
        // Incoming and doorbell participate in one sequentially consistent
        // lost-wake handshake. This RMW is ordered either before publication,
        // leaving the producer's bit set, or after publication, making the
        // subsequent incoming detach observe that node. Acquire/Release on
        // the two independent atomics permits a cross-object ordering cycle.
        self.doorbell.swap(false, Ordering::SeqCst);
    }

    fn reassert_doorbell(&self) {
        self.doorbell.store(true, Ordering::SeqCst);
    }

    fn structural_pending(&self) -> bool {
        self.backlog_pending.load(Ordering::Acquire)
            || !self.incoming.load(Ordering::SeqCst).is_null()
    }

    fn has_pending(&self) -> bool {
        self.doorbell.load(Ordering::SeqCst) || self.structural_pending()
    }

    #[cfg(feature = "workqueue")]
    fn begin_worker_install(&self) -> Result<bool, WorkQueueError> {
        match self.worker_state.compare_exchange(
            WORKER_UNINITIALIZED,
            WORKER_INSTALLING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(true),
            Err(WORKER_READY) => Ok(false),
            Err(WORKER_INSTALLING) => Err(WorkQueueError::WorkerInstalling),
            Err(_) => Err(WorkQueueError::InvalidState),
        }
    }

    #[cfg(feature = "workqueue")]
    fn cancel_worker_install(&self) {
        let previous = self
            .worker_state
            .swap(WORKER_UNINITIALIZED, Ordering::AcqRel);
        assert_eq!(
            previous, WORKER_INSTALLING,
            "workqueue worker installation rolled back from an invalid state"
        );
    }

    #[cfg(feature = "workqueue")]
    fn publish_worker(&self, thread: &ThreadHandle) {
        let wake = Box::leak(Box::new(thread.wake_handle()));
        self.worker_thread
            .store(thread.id().as_u64(), Ordering::Release);
        self.worker_wake
            .store(ptr::from_mut(wake), Ordering::Release);
        let previous = self.worker_state.swap(WORKER_READY, Ordering::AcqRel);
        assert_eq!(
            previous, WORKER_INSTALLING,
            "workqueue worker was published from an invalid install state"
        );
    }

    #[cfg(feature = "workqueue")]
    fn worker_ready(&self) -> bool {
        !self.worker_poisoned.load(Ordering::Acquire)
            && self.worker_state.load(Ordering::Acquire) == WORKER_READY
            && !self.worker_wake.load(Ordering::Acquire).is_null()
    }

    #[cfg(feature = "workqueue")]
    fn worker_id(&self) -> u64 {
        self.worker_thread.load(Ordering::Acquire)
    }

    #[cfg(feature = "workqueue")]
    fn worker_wake_handle(&self) -> Result<&'static ThreadWakeHandle, WorkQueueError> {
        if self.worker_poisoned.load(Ordering::Acquire) {
            return Err(WorkQueueError::WorkerPoisoned);
        }
        if self.worker_state.load(Ordering::Acquire) != WORKER_READY {
            return Err(WorkQueueError::WorkerNotInitialized);
        }
        let wake = self.worker_wake.load(Ordering::Acquire);
        if wake.is_null() {
            return Err(WorkQueueError::WorkerNotInitialized);
        }
        Ok(unsafe {
            // SAFETY: worker installation leaks this owning wake handle for the
            // shutdown lifetime before publishing the pointer with Release.
            &*wake
        })
    }

    #[cfg(feature = "workqueue")]
    fn poison_after_published_wake_failure(&self, wake_result: ax_task::WakeResult) -> ! {
        self.worker_poisoned.store(true, Ordering::Release);
        self.reassert_doorbell();
        panic!("published work lost its shutdown-lifetime worker wake invariant: {wake_result:?}");
    }
}

// SAFETY: producers access only atomics. `consumer_backlog` is read and written
// exclusively while `consumer_active` is owned by one `LaneConsumer`; its
// nodes have `'static` pinned storage and cannot disappear during traversal.
unsafe impl Sync for WorkerLane {}

struct LaneConsumer<'lane> {
    lane: &'lane WorkerLane,
}

impl LaneConsumer<'_> {
    fn pop(&mut self) -> Option<&'static WorkItem> {
        let mut backlog = unsafe {
            // SAFETY: construction owns the lane's single-consumer bit.
            *self.lane.consumer_backlog.get()
        };
        if backlog.is_null() {
            // Consume the detached producer snapshot directly. Reversing it
            // would traverse an unbounded number of nodes before the first
            // budgeted callback. Domains guarantee serialization, not FIFO;
            // protocols that require ordering retain it in one service item.
            backlog = self.lane.incoming.swap(ptr::null_mut(), Ordering::SeqCst);
        }
        if backlog.is_null() {
            self.lane.backlog_pending.store(false, Ordering::Release);
            return None;
        }

        let work = unsafe {
            // SAFETY: detached/backlog nodes are pinned for `'static`; only
            // this consumer can mutate their intrusive `next` links.
            &*backlog
        };
        let next = work.next.swap(ptr::null_mut(), Ordering::Relaxed);
        unsafe {
            // SAFETY: this consumer exclusively owns the backlog field.
            *self.lane.consumer_backlog.get() = next;
        }
        self.lane
            .backlog_pending
            .store(!next.is_null(), Ordering::Release);
        Some(work)
    }
}

impl Drop for LaneConsumer<'_> {
    fn drop(&mut self) {
        assert!(
            self.lane.consumer_active.swap(false, Ordering::Release),
            "workqueue consumer released without lane ownership"
        );
    }
}

fn state_flags(state: u64) -> u64 {
    state & FLAGS_MASK
}

fn state_epoch(state: u64) -> u64 {
    state >> EPOCH_SHIFT
}

fn state_route(state: u64) -> Option<WorkerRoute> {
    WorkerRoute::decode((state & ROUTE_MASK) >> ROUTE_SHIFT)
}

fn next_epoch(state: u64) -> Result<u64, WorkQueueError> {
    state_epoch(state)
        .checked_add(1)
        .filter(|epoch| *epoch <= MAX_EPOCH)
        .ok_or(WorkQueueError::EpochExhausted)
}

fn encode_state(epoch: u64, route: Option<WorkerRoute>, flags: u64) -> u64 {
    (epoch << EPOCH_SHIFT) | route.map_or(0, |route| route.encode() << ROUTE_SHIFT) | flags
}

fn idle_state(epoch: u64) -> u64 {
    encode_state(epoch, None, 0)
}

fn queued_state(epoch: u64, route: WorkerRoute) -> u64 {
    encode_state(epoch, Some(route), QUEUED)
}

fn running_state(epoch: u64, route: WorkerRoute) -> u64 {
    encode_state(epoch, Some(route), RUNNING)
}

fn running_rerun_state(epoch: u64, route: WorkerRoute) -> u64 {
    encode_state(epoch, Some(route), RUNNING | RERUN)
}
