const DELAYED_IDLE: u8 = 0;
const DELAYED_ARMED: u8 = 1;
const DELAYED_QUEUED: u8 = 2;
const DELAYED_PUBLISHING: u8 = 3;
const DELAYED_RETIRED: u8 = 4;
const DELAYED_COMMAND_CANCEL: u64 = 0;
const DELAYED_COMMAND_IMMEDIATE: u64 = 1;
const DELAYED_DEADLINE_BIAS: u64 = 2;
const DELAYED_COMMAND_GENERATION_BIT: u64 = 1 << 63;
const DELAYED_COMMAND_PAYLOAD_MASK: u64 = !DELAYED_COMMAND_GENERATION_BIT;
const DELAYED_TIMER_OWNER_CLASS: u64 = 0x4158_5751_5449_4d52;
const DELAYED_PUBLISHER_CLOSED: usize = 1 << (usize::BITS - 1);
const DELAYED_PUBLISHER_COUNT_MASK: usize = !DELAYED_PUBLISHER_CLOSED;
const RETIRED_FROM_HEAP: u8 = 1 << 0;
const RETIRED_FROM_EXPIRED_BUFFER: u8 = 1 << 1;
const RETIRED_AFTER_DISPATCH: u8 = 1 << 2;

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
        let _publisher = delayed.begin_publisher()?;
        runtime_worker_wake(WorkerRoute::new(cpu, self.priority, crate::CPU_CAPACITY)?)?;
        runtime_worker_wake(WorkerRoute::new(
            cpu,
            WorkPriority::High,
            crate::CPU_CAPACITY,
        )?)?;
        self.get_ref().enable_runtime_notifications();

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

/// Proof that a delayed item no longer has an ax-task timer or work publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "only this proof permits releasing the pinned delayed-work owner"]
pub struct DelayedWorkRetireProof {
    owner_address: usize,
    timer_generation: u64,
    timer_retire_flags: u8,
}

impl DelayedWorkRetireProof {
    /// Returns the pinned delayed-work address covered by this proof.
    pub const fn owner_address(self) -> usize {
        self.owner_address
    }

    /// Returns the final timer generation, or zero when no timer was armed.
    pub const fn timer_generation(self) -> u64 {
        self.timer_generation
    }

    /// Reports that a final timer event had already crossed the runtime hook.
    pub const fn timer_was_dispatched(self) -> bool {
        self.timer_retire_flags & RETIRED_AFTER_DISPATCH != 0
    }

    /// Reports that the final generation was removed from the timer heap.
    pub const fn removed_heap_entry(self) -> bool {
        self.timer_retire_flags & RETIRED_FROM_HEAP != 0
    }

    /// Reports that the final generation was removed from safe-point storage.
    pub const fn removed_buffered_expiration(self) -> bool {
        self.timer_retire_flags & RETIRED_FROM_EXPIRED_BUFFER != 0
    }
}

struct DelayedPublisher<'delayed> {
    delayed: &'delayed DelayedWork,
}

impl Drop for DelayedPublisher<'_> {
    fn drop(&mut self) {
        let previous = self
            .delayed
            .publication_state
            .fetch_sub(1, Ordering::Release);
        debug_assert_ne!(
            previous & DELAYED_PUBLISHER_COUNT_MASK,
            0,
            "delayed publisher count underflowed"
        );
    }
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

const DELAYED_WORK_QUARANTINE_CAPACITY: usize = 64;
const QUARANTINE_FREE: u8 = 0;
const QUARANTINE_RESERVED: u8 = 1;
const QUARANTINE_OCCUPIED: u8 = 2;

struct QuarantinedDelayedWork {
    _delayed: Pin<Box<DelayedWork>>,
    _reason: DelayedWorkQuarantineReason,
}

struct DelayedWorkQuarantineSlot {
    state: AtomicU8,
    retained: UnsafeCell<MaybeUninit<QuarantinedDelayedWork>>,
}

impl DelayedWorkQuarantineSlot {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(QUARANTINE_FREE),
            retained: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    fn reserve(&self) -> bool {
        self.state
            .compare_exchange(
                QUARANTINE_FREE,
                QUARANTINE_RESERVED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn release(&self) {
        self.state
            .compare_exchange(
                QUARANTINE_RESERVED,
                QUARANTINE_FREE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("only a live delayed-work reservation may be released");
    }

    fn retain(&self, delayed: Pin<Box<DelayedWork>>, reason: DelayedWorkQuarantineReason) {
        assert_eq!(
            self.state.load(Ordering::Acquire),
            QUARANTINE_RESERVED,
            "delayed-work quarantine slot lost its reservation"
        );
        unsafe {
            // SAFETY: a successful reservation has exactly one linear owner.
            // Occupied slots are shutdown-lived and are never reclaimed or
            // exposed, so no other context can access this storage.
            (*self.retained.get()).write(QuarantinedDelayedWork {
                _delayed: delayed,
                _reason: reason,
            });
        }
        self.state.store(QUARANTINE_OCCUPIED, Ordering::Release);
    }

    #[cfg(test)]
    fn is_occupied(&self) -> bool {
        self.state.load(Ordering::Acquire) == QUARANTINE_OCCUPIED
    }
}

// SAFETY: a slot is mutated only by the unique owner of its successful
// FREE-to-RESERVED transition. Publication is one-way to OCCUPIED, whose
// shutdown-lifetime value is never exposed or reclaimed.
unsafe impl Sync for DelayedWorkQuarantineSlot {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DelayedWorkQuarantineReason {
    DropWithoutRetire,
    RetiredWithoutRelease,
}

struct DelayedWorkQuarantineRegistry {
    slots: [DelayedWorkQuarantineSlot; DELAYED_WORK_QUARANTINE_CAPACITY],
}

impl DelayedWorkQuarantineRegistry {
    const fn new() -> Self {
        Self {
            slots: [const { DelayedWorkQuarantineSlot::new() }; DELAYED_WORK_QUARANTINE_CAPACITY],
        }
    }

    fn reserve(&self) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .find_map(|(index, slot)| slot.reserve().then_some(index))
    }

    fn release(&self, index: usize) {
        self.slots
            .get(index)
            .expect("delayed-work quarantine reservation index is valid")
            .release();
    }

    fn retain(
        &self,
        index: usize,
        delayed: Pin<Box<DelayedWork>>,
        reason: DelayedWorkQuarantineReason,
    ) {
        self.slots
            .get(index)
            .expect("delayed-work quarantine reservation index is valid")
            .retain(delayed, reason);
    }

    #[cfg(test)]
    fn occupied(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_occupied()).count()
    }
}

static DELAYED_WORK_QUARANTINE: DelayedWorkQuarantineRegistry =
    DelayedWorkQuarantineRegistry::new();

struct DelayedWorkQuarantineReservation {
    index: Option<usize>,
}

impl DelayedWorkQuarantineReservation {
    fn reserve() -> Result<Self, WorkQueueError> {
        let index = DELAYED_WORK_QUARANTINE
            .reserve()
            .ok_or(WorkQueueError::DelayedWorkQuarantineCapacity)?;
        Ok(Self { index: Some(index) })
    }

    fn release(mut self) {
        let index = self
            .index
            .take()
            .expect("delayed-work quarantine reservation is live");
        DELAYED_WORK_QUARANTINE.release(index);
    }

    fn retain(mut self, delayed: Pin<Box<DelayedWork>>, reason: DelayedWorkQuarantineReason) {
        let index = self
            .index
            .take()
            .expect("delayed-work quarantine reservation is live");
        DELAYED_WORK_QUARANTINE.retain(index, delayed, reason);
    }
}

impl Drop for DelayedWorkQuarantineReservation {
    fn drop(&mut self) {
        if let Some(index) = self.index.take() {
            DELAYED_WORK_QUARANTINE.release(index);
        }
    }
}

/// One pinned delayed activation backed by an ax-task timer and two fixed work nodes.
///
/// `control_work` always runs on the target CPU's high-priority shared worker;
/// `work` runs on the caller-selected logical queue. A raw item must stay pinned
/// until [`Self::shutdown_sync`] succeeds. [`DelayedWorkRegistration`] enforces
/// that rule by retaining any unretired owner in named quarantine.
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
    publication_state: AtomicUsize,
    retired_timer_generation: AtomicU64,
    retired_timer_flags: AtomicU8,
    _pin: PhantomPinned,
}

/// Owned delayed-work registration with fail-closed Drop behavior.
///
/// The registration pre-reserves quarantine capacity before it can publish a
/// raw intrusive pointer. [`Self::shutdown_sync`] is the only path that proves
/// protocol retirement; [`RetiredDelayedWork::release`] then validates the
/// release context. Abandoning either stage moves the complete pinned owner
/// into named fixed-capacity quarantine without cancelling timers, fabricating
/// completion, freeing memory, or taking a shared lock.
#[must_use = "explicitly retire the registration or its pinned owner is quarantined"]
pub struct DelayedWorkRegistration {
    queue: Pin<&'static WorkQueue>,
    delayed: Option<Pin<Box<DelayedWork>>>,
    quarantine: Option<DelayedWorkQuarantineReservation>,
}

/// Retired delayed-work allocation awaiting an explicit task-context release.
///
/// Retirement proves that no timer, safe-point event, or workqueue publication
/// can still reach the allocation. It deliberately does not infer whether the
/// caller is allowed to free memory. [`Self::release`] performs that separate
/// context check; dropping this value instead retains the allocation in named
/// quarantine without consulting uninitialized CPU-local runtime state.
#[must_use = "explicitly release the retired allocation or it is quarantined"]
pub struct RetiredDelayedWork {
    delayed: Option<Pin<Box<DelayedWork>>>,
    proof: DelayedWorkRetireProof,
    quarantine: Option<DelayedWorkQuarantineReservation>,
}

impl RetiredDelayedWork {
    /// Returns the complete timer/work retirement proof.
    pub const fn proof(&self) -> DelayedWorkRetireProof {
        self.proof
    }

    /// Releases the retired allocation from an ordinary schedulable context.
    ///
    /// This operation performs no timer, IRQ, or workqueue protocol. Those
    /// obligations were discharged by the retirement proof. On a context
    /// error ownership is returned intact, so Drop can still fail closed.
    pub fn release(mut self) -> Result<(), (Self, WorkQueueError)> {
        if let Err(error) = ensure_runtime_release_context() {
            return Err((self, error));
        }
        let delayed = self
            .delayed
            .take()
            .expect("retired delayed work owns its allocation");
        let reservation = self
            .quarantine
            .take()
            .expect("retired delayed work retains its quarantine reservation");
        reservation.release();
        drop(delayed);
        Ok(())
    }
}

impl core::fmt::Debug for RetiredDelayedWork {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RetiredDelayedWork")
            .field(
                "owner_address",
                &self
                    .delayed
                    .as_ref()
                    .map(|delayed| ptr::from_ref(delayed.as_ref().get_ref()).expose_provenance()),
            )
            .field("proof", &self.proof)
            .finish()
    }
}

impl DelayedWorkRegistration {
    /// Allocates one pinned delayed item after reserving fail-closed capacity.
    pub fn new(
        queue: Pin<&'static WorkQueue>,
        callback: fn(usize) -> WorkOutcome,
        callback_data: usize,
    ) -> Result<Self, WorkQueueError> {
        let quarantine = DelayedWorkQuarantineReservation::reserve()?;
        Ok(Self {
            queue,
            delayed: Some(Box::pin(DelayedWork::new(callback, callback_data))),
            quarantine: Some(quarantine),
        })
    }

    /// Arms or moves this owned delayed activation.
    pub fn mod_delayed_work_on(
        &self,
        delay_ns: u64,
    ) -> Result<ModDelayedWorkResult, WorkQueueError> {
        self.queue
            .mod_delayed_work_on(self.queue.cpu(), self.delayed_pin(), delay_ns)
    }

    /// Returns and clears the most recent asynchronous timer-control failure.
    pub fn take_failure(&self) -> Option<DelayedWorkFailure> {
        self.delayed_ref().take_failure()
    }

    /// Explicitly joins all timer/work publications and restores ordinary Drop.
    pub fn shutdown_sync(mut self) -> Result<RetiredDelayedWork, (Self, WorkQueueError)> {
        let proof = match self.delayed_pin().shutdown_sync(self.queue) {
            Ok(proof) => proof,
            Err(error) => return Err((self, error)),
        };
        let delayed = self
            .delayed
            .take()
            .expect("live registration owns its delayed allocation");
        let quarantine = self
            .quarantine
            .take()
            .expect("live registration owns quarantine capacity");
        Ok(RetiredDelayedWork {
            delayed: Some(delayed),
            proof,
            quarantine: Some(quarantine),
        })
    }

    fn delayed_ref(&self) -> &DelayedWork {
        self.delayed
            .as_ref()
            .expect("live registration owns its delayed allocation")
            .as_ref()
            .get_ref()
    }

    fn delayed_pin(&self) -> Pin<&'static DelayedWork> {
        let delayed = ptr::from_ref(self.delayed_ref());
        unsafe {
            // SAFETY: the allocation is pinned. Explicit retirement is the
            // only ordinary-release path; otherwise Drop transfers the Box to
            // pre-reserved shutdown-lifetime quarantine.
            Pin::new_unchecked(&*delayed)
        }
    }
}

impl Drop for DelayedWorkRegistration {
    fn drop(&mut self) {
        let Some(delayed) = self.delayed.take() else {
            return;
        };
        let reservation = self
            .quarantine
            .take()
            .expect("live delayed registration owns quarantine capacity");
        reservation.retain(delayed, DelayedWorkQuarantineReason::DropWithoutRetire);
    }
}

impl Drop for RetiredDelayedWork {
    fn drop(&mut self) {
        let Some(delayed) = self.delayed.take() else {
            return;
        };
        let reservation = self
            .quarantine
            .take()
            .expect("retired delayed work retains its quarantine reservation");
        reservation.retain(delayed, DelayedWorkQuarantineReason::RetiredWithoutRelease);
    }
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
            publication_state: AtomicUsize::new(0),
            retired_timer_generation: AtomicU64::new(0),
            retired_timer_flags: AtomicU8::new(0),
            _pin: PhantomPinned,
        }
    }

    /// Synchronously retires the timer, safe-point event, and both work nodes.
    ///
    /// New deadline publishers are closed first. The owner-CPU control work
    /// then physically detaches the timer generation from ax-task, after which
    /// queued/running user work is cancelled and joined. Drop is intentionally
    /// not involved in this protocol.
    pub fn shutdown_sync(
        self: Pin<&'static Self>,
        queue: Pin<&'static WorkQueue>,
    ) -> Result<DelayedWorkRetireProof, WorkQueueError> {
        let delayed = self.get_ref();
        if delayed.is_never_submitted() {
            delayed.close_publishers()?;
            return delayed.finish_retirement();
        }
        delayed.ensure_queue(queue.get_ref())?;
        ensure_runtime_wait_context(self.control_work().get_ref())?;
        delayed.close_publishers()?;

        let result = (|| {
            while delayed.publication_state.load(Ordering::Acquire) != DELAYED_PUBLISHER_CLOSED {
                let _decision = yield_current_cpu()?;
            }
            queue.cancel_delayed_work_sync(self)?;
            delayed.finish_queued_idle();
            delayed.finish_retirement()
        })();
        if result.is_err() {
            let previous = delayed
                .publication_state
                .fetch_and(DELAYED_PUBLISHER_COUNT_MASK, Ordering::Release);
            debug_assert_eq!(previous, DELAYED_PUBLISHER_CLOSED);
        }
        result
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

    fn begin_publisher(&self) -> Result<DelayedPublisher<'_>, WorkQueueError> {
        let mut observed = self.publication_state.load(Ordering::Acquire);
        loop {
            if observed & DELAYED_PUBLISHER_CLOSED != 0
                || observed & DELAYED_PUBLISHER_COUNT_MASK == DELAYED_PUBLISHER_COUNT_MASK
            {
                return Err(WorkQueueError::InvalidState);
            }
            match self.publication_state.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(DelayedPublisher { delayed: self }),
                Err(current) => observed = current,
            }
        }
    }

    fn close_publishers(&self) -> Result<(), WorkQueueError> {
        let previous = self
            .publication_state
            .fetch_or(DELAYED_PUBLISHER_CLOSED, Ordering::AcqRel);
        if previous & DELAYED_PUBLISHER_CLOSED == 0 {
            Ok(())
        } else {
            Err(WorkQueueError::InvalidState)
        }
    }

    fn finish_retirement(&self) -> Result<DelayedWorkRetireProof, WorkQueueError> {
        if !self.control_work.is_idle()
            || !self.work.is_idle()
            || self.armed_token.load(Ordering::Acquire) != 0
        {
            return Err(WorkQueueError::InvalidState);
        }
        self.phase
            .compare_exchange(
                DELAYED_IDLE,
                DELAYED_RETIRED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| WorkQueueError::InvalidState)?;
        Ok(DelayedWorkRetireProof {
            owner_address: self.owner_address.load(Ordering::Acquire),
            timer_generation: self.retired_timer_generation.load(Ordering::Acquire),
            timer_retire_flags: self.retired_timer_flags.load(Ordering::Acquire),
        })
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

    fn record_timer_retire(&self, proof: TimerRetireProof) {
        let mut flags = 0;
        if proof.removed_heap_entry() {
            flags |= RETIRED_FROM_HEAP;
        }
        if proof.removed_buffered_expiration() {
            flags |= RETIRED_FROM_EXPIRED_BUFFER;
        }
        self.retired_timer_flags.store(flags, Ordering::Relaxed);
        self.retired_timer_generation
            .store(proof.token().generation(), Ordering::Release);
    }

    fn record_dispatched_timer(&self, generation: u64) {
        self.retired_timer_flags
            .store(RETIRED_AFTER_DISPATCH, Ordering::Relaxed);
        self.retired_timer_generation
            .store(generation, Ordering::Release);
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
            // SAFETY: DelayedWork stays pinned through explicit retirement or
            // quarantine, so its embedded intrusive WorkItem cannot move.
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
            // SAFETY: delayed work stays pinned through retirement or quarantine.
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
    }
}

#[cfg(feature = "workqueue")]
fn delayed_control_entry(data: usize) -> WorkOutcome {
    let delayed = unsafe {
        // SAFETY: `bind` publishes only the address of a DelayedWork pinned
        // through explicit retirement or quarantine.
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
                // SAFETY: the owner remains pinned until retirement completes.
                Pin::new_unchecked(&self.timer)
            };
            match retire_current_runtime_timer(timer, token) {
                Ok(proof) => self.record_timer_retire(proof),
                Err(_) => {
                    self.publish_failure_activation(DelayedWorkFailure::Runtime);
                    return WorkOutcome::Complete;
                }
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
            // SAFETY: binding published this retirement-gated pinned object,
            // and the non-zero class uniquely selects DelayedWork in runtime.
            RuntimeTimerOwner::new(owner_address, DELAYED_TIMER_OWNER_CLASS)
        };
        let timer = unsafe {
            // SAFETY: callback-data binding proves retirement-gated pinning.
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
        if token != 0 {
            self.record_dispatched_timer(token);
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

    #[test]
    fn retirement_proof_requires_timer_control_and_callback_publications_to_be_idle() {
        let delayed = Box::pin(DelayedWork::new(complete_work, 0));
        let delayed = delayed.as_ref().get_ref();
        delayed.owner_address.store(0x1000, Ordering::Release);
        delayed.record_dispatched_timer(7);

        delayed.armed_token.store(8, Ordering::Release);
        assert_eq!(
            delayed.finish_retirement(),
            Err(WorkQueueError::InvalidState)
        );
        delayed.armed_token.store(0, Ordering::Release);

        delayed.control_work.state.store(QUEUED, Ordering::Release);
        assert_eq!(
            delayed.finish_retirement(),
            Err(WorkQueueError::InvalidState)
        );
        delayed.control_work.state.store(0, Ordering::Release);

        delayed.work.state.store(RUNNING, Ordering::Release);
        assert_eq!(
            delayed.finish_retirement(),
            Err(WorkQueueError::InvalidState)
        );
        delayed.work.state.store(0, Ordering::Release);

        let proof = delayed.finish_retirement().unwrap();
        assert_eq!(proof.owner_address(), 0x1000);
        assert_eq!(proof.timer_generation(), 7);
        assert!(proof.timer_was_dispatched());
        assert_eq!(delayed.phase.load(Ordering::Acquire), DELAYED_RETIRED);
    }

    #[test]
    fn quarantine_registry_retains_the_complete_pinned_owner() {
        let registry = DelayedWorkQuarantineRegistry::new();
        let slot = registry.reserve().unwrap();
        registry.retain(
            slot,
            Box::pin(DelayedWork::new(complete_work, 0)),
            DelayedWorkQuarantineReason::DropWithoutRetire,
        );

        assert_eq!(registry.occupied(), 1);
    }

    #[test]
    fn never_published_owned_registration_drop_is_fail_closed_after_retirement() {
        let queue_ref: &'static WorkQueue =
            Box::leak(Box::new(WorkQueue::new(0, WorkPriority::High)));
        let queue = unsafe {
            // SAFETY: the test queue remains fixed for the process lifetime.
            Pin::new_unchecked(queue_ref)
        };
        let registration = DelayedWorkRegistration::new(queue, complete_work, 0).unwrap();

        let retired = match registration.shutdown_sync() {
            Ok(retired) => retired,
            Err((_registration, error)) => panic!("idle retirement failed: {error}"),
        };

        assert_eq!(retired.proof().owner_address(), 0);
        let before = DELAYED_WORK_QUARANTINE.occupied();
        drop(retired);
        assert_eq!(DELAYED_WORK_QUARANTINE.occupied(), before + 1);
    }

    #[test]
    fn dropping_a_live_registration_retains_its_owner_without_teardown() {
        let queue_ref: &'static WorkQueue =
            Box::leak(Box::new(WorkQueue::new(0, WorkPriority::High)));
        let queue = unsafe {
            // SAFETY: the test queue remains fixed for the process lifetime.
            Pin::new_unchecked(queue_ref)
        };
        let registration = DelayedWorkRegistration::new(queue, complete_work, 0).unwrap();
        let owner_address = ptr::from_ref(registration.delayed_ref()).expose_provenance();
        registration
            .delayed_ref()
            .owner_address
            .store(owner_address, Ordering::Release);
        let before = DELAYED_WORK_QUARANTINE.occupied();

        drop(registration);

        assert_eq!(DELAYED_WORK_QUARANTINE.occupied(), before + 1);
    }
}
