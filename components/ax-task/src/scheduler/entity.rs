//! Class-specific mutable state stored with each thread.

use alloc::boxed::Box;
#[cfg(feature = "task-test-hooks")]
use core::sync::atomic::AtomicU64;
use core::{
    ops::{Deref, DerefMut},
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{DeadlineEntity, DeadlineServer, FairEntity, SchedulePolicy, SchedulingUrgency};

/// Stable unique owner of one task's complete class accounting.
///
/// Linux embeds the Fair, RT, and Deadline entities in `task_struct`; changing
/// effective priority never transfers the configured-policy entity to a
/// second owner or copies it through `rq->curr`. This handle provides the same
/// stable-record ownership rule without intrusive self references: one box
/// moves between task control and the owner rq while the complete scheduler
/// state stays at one address.
#[derive(Debug)]
pub(crate) struct ActiveSchedulingState {
    record: Box<ActiveSchedulingRecord>,
}

/// Complete class accounting behind one stable ownership handle.
#[derive(Debug)]
struct ActiveSchedulingRecord {
    effective_policy: SchedulePolicy,
    base_entity: SchedulingEntity,
    inherited_entity: Option<SchedulingEntity>,
}

/// Task-stable owner slot used while an entity is detached from every rq.
///
/// Linux keeps each scheduling entity embedded in `task_struct`. Ax-task's
/// class queues still transfer one move-only owner handle, but the detached
/// handle always returns to this task-resident slot. The slot is atomic only
/// for the short `Parking -> Blocked` publication: ordinary readers and
/// writers additionally hold the task scheduler lock.
#[derive(Debug)]
pub(crate) struct DetachedActiveState {
    record: AtomicPtr<ActiveSchedulingRecord>,
}

/// Exclusive task-lock borrow of one detached scheduling entity.
pub(crate) struct DetachedActiveGuard<'a> {
    slot: &'a DetachedActiveState,
    active: Option<ActiveSchedulingState>,
}

/// Move-only reservation for rq-owned block publication.
///
/// The marker makes a task-lock reader wait until rq removal and placement
/// publication are complete. Dropping an unfinished reservation restores the
/// empty slot, which is required when a racing wake cancels the park CAS.
pub(crate) struct DetachedActivePublication<'a> {
    slot: &'a DetachedActiveState,
    completed: bool,
}

static DETACHED_ACTIVE_PUBLISHING: u8 = 0;

#[cfg(feature = "task-test-hooks")]
static DETACHED_PUBLICATION_WAITS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "task-test-hooks")]
static DETACHED_PUBLICATION_WAIT_ITERATIONS: AtomicU64 = AtomicU64::new(0);

impl ActiveSchedulingState {
    pub(crate) fn new(policy: SchedulePolicy, entity: SchedulingEntity) -> Self {
        Self {
            record: Box::new(ActiveSchedulingRecord {
                effective_policy: policy,
                base_entity: entity,
                inherited_entity: None,
            }),
        }
    }

    pub(crate) fn policy(&self) -> SchedulePolicy {
        self.record.effective_policy
    }

    pub(crate) fn entity(&self) -> &SchedulingEntity {
        self.record
            .inherited_entity
            .as_ref()
            .unwrap_or(&self.record.base_entity)
    }

    pub(crate) fn entity_mut(&mut self) -> &mut SchedulingEntity {
        self.record
            .inherited_entity
            .as_mut()
            .unwrap_or(&mut self.record.base_entity)
    }

    pub(crate) fn base_entity(&self) -> &SchedulingEntity {
        &self.record.base_entity
    }

    pub(crate) fn base_entity_mut(&mut self) -> &mut SchedulingEntity {
        &mut self.record.base_entity
    }

    pub(crate) fn replace_base_entity(&mut self, entity: SchedulingEntity) {
        self.record.base_entity = entity;
    }

    pub(crate) fn uses_inherited_entity(&self) -> bool {
        self.record.inherited_entity.is_some()
    }

    /// Makes the configured-policy entity effective again after PI deboost.
    pub(crate) fn use_base_entity(&mut self, policy: SchedulePolicy) {
        self.record.inherited_entity = None;
        self.record.effective_policy = policy;
    }

    /// Changes only the effective policy/key while retaining base accounting.
    ///
    /// This is used for same-class PI. In particular, an RR task keeps its
    /// remaining quantum while inheriting an RT priority.
    pub(crate) fn use_base_entity_with_effective_policy(&mut self, policy: SchedulePolicy) {
        debug_assert!(self.record.inherited_entity.is_none());
        self.record.effective_policy = policy;
    }

    /// Installs the class-specific entity used by a cross-class PI boost.
    pub(crate) fn use_inherited_entity(
        &mut self,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
    ) {
        self.record.inherited_entity = Some(entity);
        self.record.effective_policy = policy;
    }

    pub(crate) fn update_inherited_effective_policy(&mut self, policy: SchedulePolicy) {
        debug_assert!(self.record.inherited_entity.is_some());
        self.record.effective_policy = policy;
    }

    fn into_raw(self) -> *mut ActiveSchedulingRecord {
        Box::into_raw(self.record)
    }

    /// # Safety
    ///
    /// `record` must have been returned by [`Self::into_raw`], must still be
    /// uniquely owned, and must not be the detached-publication marker.
    unsafe fn from_raw(record: *mut ActiveSchedulingRecord) -> Self {
        debug_assert!(!record.is_null());
        debug_assert_ne!(record, detached_active_publication_marker());
        Self {
            // SAFETY: guaranteed by this function's ownership contract.
            record: unsafe { Box::from_raw(record) },
        }
    }
}

fn detached_active_publication_marker() -> *mut ActiveSchedulingRecord {
    ptr::addr_of!(DETACHED_ACTIVE_PUBLISHING).cast_mut().cast()
}

impl DetachedActiveState {
    pub(crate) fn new(active: ActiveSchedulingState) -> Self {
        Self {
            record: AtomicPtr::new(active.into_raw()),
        }
    }

    /// Waits for an rq-owned park publication to either finish or roll back.
    pub(crate) fn wait_for_publication(&self) {
        #[cfg(feature = "task-test-hooks")]
        let mut wait_iterations = 0u64;
        while self.record.load(Ordering::Acquire) == detached_active_publication_marker() {
            #[cfg(feature = "task-test-hooks")]
            {
                wait_iterations = wait_iterations.saturating_add(1);
            }
            core::hint::spin_loop();
        }
        #[cfg(feature = "task-test-hooks")]
        record_detached_publication_wait(wait_iterations);
    }

    pub(crate) fn publication_in_progress(&self) -> bool {
        self.record.load(Ordering::Acquire) == detached_active_publication_marker()
    }

    /// Borrows the detached entity while the caller holds the task lock.
    pub(crate) fn active(&self) -> DetachedActiveGuard<'_> {
        DetachedActiveGuard {
            slot: self,
            active: Some(
                self.take()
                    .expect("detached task must own its active scheduling state"),
            ),
        }
    }

    /// Borrows the detached entity if it is not owned by an rq.
    pub(crate) fn active_option(&self) -> Option<DetachedActiveGuard<'_>> {
        self.take().map(|active| DetachedActiveGuard {
            slot: self,
            active: Some(active),
        })
    }

    /// Transfers detached ownership into an rq node or current dispatch.
    pub(crate) fn take(&self) -> Option<ActiveSchedulingState> {
        #[cfg(feature = "task-test-hooks")]
        let mut wait_iterations = 0u64;
        loop {
            let record = self.record.load(Ordering::Acquire);
            if record == detached_active_publication_marker() {
                #[cfg(feature = "task-test-hooks")]
                {
                    wait_iterations = wait_iterations.saturating_add(1);
                }
                core::hint::spin_loop();
                continue;
            }
            if record.is_null() {
                #[cfg(feature = "task-test-hooks")]
                record_detached_publication_wait(wait_iterations);
                return None;
            }
            if self
                .record
                .compare_exchange(record, ptr::null_mut(), Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                // SAFETY: the successful CAS transferred the slot's unique
                // Box ownership to this return value.
                #[cfg(feature = "task-test-hooks")]
                record_detached_publication_wait(wait_iterations);
                return Some(unsafe { ActiveSchedulingState::from_raw(record) });
            }
        }
    }

    /// Returns rq/current ownership to the task-stable detached slot.
    pub(crate) fn install(&self, active: ActiveSchedulingState) {
        let record = active.into_raw();
        if let Err(existing) = self.record.compare_exchange(
            ptr::null_mut(),
            record,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            // SAFETY: the failed CAS leaves ownership with this call.
            drop(unsafe { ActiveSchedulingState::from_raw(record) });
            panic!("active scheduling state cannot have two owners (slot={existing:p})");
        }
    }

    /// Reserves the empty detached slot before publishing `Blocked`.
    pub(crate) fn begin_publication(&self) -> Option<DetachedActivePublication<'_>> {
        self.record
            .compare_exchange(
                ptr::null_mut(),
                detached_active_publication_marker(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| DetachedActivePublication {
                slot: self,
                completed: false,
            })
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn owns_active(&self) -> bool {
        let record = self.record.load(Ordering::Acquire);
        !record.is_null() && record != detached_active_publication_marker()
    }
}

#[cfg(feature = "task-test-hooks")]
fn record_detached_publication_wait(wait_iterations: u64) {
    if wait_iterations == 0 {
        return;
    }
    DETACHED_PUBLICATION_WAITS.fetch_add(1, Ordering::Relaxed);
    DETACHED_PUBLICATION_WAIT_ITERATIONS.fetch_add(wait_iterations, Ordering::Relaxed);
}

#[cfg(feature = "task-test-hooks")]
pub(crate) fn take_detached_publication_waits() -> (u64, u64) {
    (
        DETACHED_PUBLICATION_WAITS.swap(0, Ordering::AcqRel),
        DETACHED_PUBLICATION_WAIT_ITERATIONS.swap(0, Ordering::AcqRel),
    )
}

impl Drop for DetachedActiveState {
    fn drop(&mut self) {
        let record = *self.record.get_mut();
        assert_ne!(
            record,
            detached_active_publication_marker(),
            "task cannot be destroyed during detached entity publication"
        );
        if !record.is_null() {
            // SAFETY: exclusive `&mut self` proves that the slot still owns
            // this record and no atomic borrower can exist.
            drop(unsafe { ActiveSchedulingState::from_raw(record) });
        }
    }
}

impl Deref for DetachedActiveGuard<'_> {
    type Target = ActiveSchedulingState;

    fn deref(&self) -> &Self::Target {
        self.active
            .as_ref()
            .expect("detached active guard must retain its owner")
    }
}

impl DerefMut for DetachedActiveGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.active
            .as_mut()
            .expect("detached active guard must retain its owner")
    }
}

impl Drop for DetachedActiveGuard<'_> {
    fn drop(&mut self) {
        self.slot.install(
            self.active
                .take()
                .expect("detached active guard must return its owner"),
        );
    }
}

impl DetachedActivePublication<'_> {
    /// Publishes the entity only after rq removal and `on_rq = NONE` are
    /// visible. An Acquire observation of the pointer therefore cannot
    /// misclassify this RT task as Linux Fair delayed-dequeue state.
    pub(crate) fn finish(mut self, active: ActiveSchedulingState) {
        let record = active.into_raw();
        if self
            .slot
            .record
            .compare_exchange(
                detached_active_publication_marker(),
                record,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            // SAFETY: a failed publication leaves ownership with this call.
            drop(unsafe { ActiveSchedulingState::from_raw(record) });
            panic!("detached active publication lost its reservation");
        }
        self.completed = true;
    }
}

impl Drop for DetachedActivePublication<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if self
            .slot
            .record
            .compare_exchange(
                detached_active_publication_marker(),
                ptr::null_mut(),
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            panic!("detached active publication rollback lost its reservation");
        }
    }
}

/// Mutable scheduler accounting owned by one thread record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SchedulingEntity {
    /// Runtime-owned CPU-stopper work has no budget accounting.
    KernelStop,
    /// EEVDF fair accounting.
    Fair(FairEntity),
    /// FIFO needs only queue ordering state.
    Fifo,
    /// Round-robin preserves remaining quantum across higher-priority preemption.
    RoundRobin {
        /// Remaining quantum in nanoseconds.
        remaining_quantum_ns: u64,
    },
    /// EDF and CBS Deadline accounting.
    Deadline(DeadlineEntity),
}

impl SchedulingEntity {
    /// Creates class-specific state for a base policy.
    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub fn new(policy: SchedulePolicy, fair_slice_ns: u64, virtual_time: u64) -> Self {
        Self::new_with_deadline_server(
            policy,
            fair_slice_ns,
            virtual_time,
            DeadlineServer::unbound(),
        )
    }

    pub(crate) fn new_with_deadline_server(
        policy: SchedulePolicy,
        fair_slice_ns: u64,
        virtual_time: u64,
        deadline_server: DeadlineServer,
    ) -> Self {
        match policy {
            SchedulePolicy::KernelStop => Self::KernelStop,
            SchedulePolicy::Fair { nice, mode } => {
                Self::Fair(FairEntity::new(nice, mode, fair_slice_ns, virtual_time))
            }
            SchedulePolicy::Fifo { .. } => Self::Fifo,
            SchedulePolicy::RoundRobin { quantum_ns, .. } => Self::RoundRobin {
                remaining_quantum_ns: quantum_ns,
            },
            SchedulePolicy::Deadline(policy) => {
                Self::Deadline(DeadlineEntity::from_task_server(policy, deadline_server))
            }
        }
    }

    pub(crate) fn capture_fair_sleep_lag(&mut self, virtual_time: u64, timing_granularity_ns: u64) {
        if let Self::Fair(entity) = self {
            entity.capture_sleep_lag(virtual_time, timing_granularity_ns);
        }
    }

    pub(crate) fn capture_fair_migration(&mut self, virtual_time: u64, timing_granularity_ns: u64) {
        if let Self::Fair(entity) = self {
            entity.capture_migration(virtual_time, timing_granularity_ns);
        }
    }

    pub(crate) fn cancel_fair_migration(&mut self) {
        if let Self::Fair(entity) = self {
            entity.cancel_migration();
        }
    }

    /// Charges one dispatch and reports whether its class slice expired.
    pub fn charge(&mut self, runtime_ns: u64, virtual_time: u64, reclaimed_ns: u64) -> bool {
        match self {
            Self::KernelStop => false,
            Self::Fair(entity) => entity.charge(runtime_ns, virtual_time),
            Self::Fifo => false,
            Self::RoundRobin {
                remaining_quantum_ns,
            } => {
                *remaining_quantum_ns = remaining_quantum_ns.saturating_sub(runtime_ns);
                *remaining_quantum_ns == 0
            }
            Self::Deadline(entity) => entity.charge(runtime_ns, reclaimed_ns),
        }
    }

    /// Returns an absolute Deadline key when this is a Deadline entity.
    pub fn activate_deadline(&mut self, now_ns: u64) -> Option<u64> {
        match self {
            Self::Deadline(entity) => {
                entity.activate(now_ns);
                if entity.is_throttled() {
                    None
                } else {
                    entity.absolute_deadline_ns()
                }
            }
            _ => None,
        }
    }

    /// Returns the EEVDF entity when this is a fair thread.
    pub const fn fair(&self) -> Option<FairEntity> {
        match self {
            Self::Fair(entity) => Some(*entity),
            _ => None,
        }
    }

    /// Returns the CBS entity when this is a Deadline thread.
    pub const fn deadline(&self) -> Option<&DeadlineEntity> {
        match self {
            Self::Deadline(entity) => Some(entity),
            _ => None,
        }
    }

    /// Returns Deadline flags owned by the executing task. PI donor flags are
    /// reservation parameters only and must not grant reclaim or redirect
    /// overrun notification.
    pub fn deadline_owner_flags(&self) -> crate::DeadlineFlags {
        match self {
            Self::Deadline(entity) => entity.owner_flags(),
            _ => crate::DeadlineFlags::NONE,
        }
    }

    /// Reports whether this accounting representation matches a policy class.
    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub const fn matches_policy(&self, policy: SchedulePolicy) -> bool {
        matches!(
            (self, policy),
            (Self::KernelStop, SchedulePolicy::KernelStop)
                | (Self::Fair(_), SchedulePolicy::Fair { .. })
                | (Self::Fifo, SchedulePolicy::Fifo { .. })
                | (Self::RoundRobin { .. }, SchedulePolicy::RoundRobin { .. })
                | (Self::Deadline(_), SchedulePolicy::Deadline(_))
        )
    }

    /// Reports whether a round-robin dispatch consumed its complete quantum.
    pub const fn round_robin_quantum_expired(&self) -> bool {
        matches!(
            self,
            Self::RoundRobin {
                remaining_quantum_ns: 0
            }
        )
    }

    /// Starts a fresh round-robin quantum after yield or expiration.
    pub fn reset_round_robin_quantum(&mut self, policy: SchedulePolicy) {
        if let (
            Self::RoundRobin {
                remaining_quantum_ns,
            },
            SchedulePolicy::RoundRobin { quantum_ns, .. },
        ) = (self, policy)
        {
            *remaining_quantum_ns = quantum_ns;
        }
    }

    /// Returns whether an exhausted Deadline entity is throttled.
    pub fn is_deadline_throttled(&self) -> bool {
        matches!(self, Self::Deadline(entity) if entity.is_throttled())
    }

    /// Ends the active Deadline job and keeps it throttled until replenishment.
    pub(crate) fn yield_deadline_job(&mut self) -> bool {
        let Self::Deadline(entity) = self else {
            return false;
        };
        entity.yield_job();
        true
    }

    /// Builds PI urgency without a thread or arrival tie-break.
    pub fn scheduling_urgency(&self, policy: SchedulePolicy) -> SchedulingUrgency {
        match self {
            Self::Deadline(deadline) => deadline.scheduling_urgency(),
            _ => policy.scheduling_urgency(),
        }
    }
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use super::*;
    use crate::{DeadlineFlags, DeadlinePolicy, FairMode, Nice};

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn fair_service_request_expires_after_cumulative_small_charges() {
        let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let mut entity = SchedulingEntity::new(policy, 100, 0);

        assert!(!entity.charge(40, 0, 0));
        assert!(!entity.charge(40, 0, 0));
        assert!(entity.charge(20, 0, 0));
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn deadline_urgency_uses_the_active_absolute_deadline() {
        let policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 10, 20, DeadlineFlags::NONE).unwrap());
        let mut earlier = SchedulingEntity::new(policy, 1, 0);
        let mut later = SchedulingEntity::new(policy, 1, 0);
        earlier.activate_deadline(100);
        later.activate_deadline(200);

        assert!(earlier.scheduling_urgency(policy) < later.scheduling_urgency(policy));
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn deadline_urgency_orders_across_linux_rq_clock_wrap() {
        let earlier_policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 4, 20, DeadlineFlags::NONE).unwrap());
        let later_policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 10, 20, DeadlineFlags::NONE).unwrap());
        let mut earlier = SchedulingEntity::new(earlier_policy, 1, 0);
        let mut later = SchedulingEntity::new(later_policy, 1, 0);
        let now = u64::MAX - 5;
        earlier.activate_deadline(now);
        later.activate_deadline(now);

        assert_eq!(
            earlier.deadline().unwrap().absolute_deadline_ns(),
            Some(u64::MAX - 1)
        );
        assert_eq!(later.deadline().unwrap().absolute_deadline_ns(), Some(4));
        assert!(
            earlier.scheduling_urgency(earlier_policy) < later.scheduling_urgency(later_policy)
        );
    }
}
