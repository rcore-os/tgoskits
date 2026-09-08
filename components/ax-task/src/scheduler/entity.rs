//! Class-specific mutable state stored with each thread.

use alloc::boxed::Box;
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
/// publication are complete. The completed transition either installs an
/// off-rq entity or leaves the slot empty while a delayed Fair node retains rq
/// ownership. Dropping an unfinished reservation restores the empty slot,
/// which is required when a racing wake cancels the park CAS.
pub(crate) struct DetachedActivePublication<'a> {
    slot: &'a DetachedActiveState,
    completed: bool,
}

static DETACHED_ACTIVE_PUBLISHING: u8 = 0;

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

    pub(crate) fn policy_ref(&self) -> &SchedulePolicy {
        &self.record.effective_policy
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
        while self.record.load(Ordering::Acquire) == detached_active_publication_marker() {
            core::hint::spin_loop();
        }
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
        loop {
            let record = self.record.load(Ordering::Acquire);
            if record == detached_active_publication_marker() {
                core::hint::spin_loop();
                continue;
            }
            if record.is_null() {
                return None;
            }
            if self
                .record
                .compare_exchange(record, ptr::null_mut(), Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                // SAFETY: the successful CAS transferred the slot's unique
                // Box ownership to this return value.
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

    /// Completes publication while a delayed Fair rq node owns the entity.
    pub(crate) fn finish_rq_owned(mut self) {
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
            panic!("rq-owned active publication lost its reservation");
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

    pub(crate) fn capture_fair_sleep_lag(
        &mut self,
        virtual_time: u64,
        rq_max_slice_ns: u64,
        timing_granularity_ns: u64,
    ) {
        if let Self::Fair(entity) = self {
            entity.capture_sleep_lag(virtual_time, rq_max_slice_ns, timing_granularity_ns);
        }
    }

    pub(crate) fn capture_fair_migration(
        &mut self,
        virtual_time: u64,
        rq_max_slice_ns: u64,
        timing_granularity_ns: u64,
    ) {
        if let Self::Fair(entity) = self {
            entity.capture_migration(virtual_time, rq_max_slice_ns, timing_granularity_ns);
        }
    }

    /// Charges one dispatch and reports whether its class slice expired.
    pub fn charge(&mut self, runtime_ns: u64, virtual_time: u64, reclaimed_ns: u64) -> bool {
        match self {
            Self::KernelStop => false,
            Self::Fair(entity) => entity.charge(runtime_ns, virtual_time),
            Self::Fifo => false,
            // Linux advances SCHED_RR's time slice only from task_tick_rt().
            // Execution accounting between periodic ticks must not consume it.
            Self::RoundRobin { .. } => false,
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

    /// Advances one Linux periodic tick for a round-robin dispatch.
    pub(crate) fn advance_round_robin_tick(&mut self, tick_ns: u64) -> bool {
        assert!(tick_ns > 0, "round-robin tick duration must be nonzero");
        let Self::RoundRobin {
            remaining_quantum_ns,
        } = self
        else {
            return false;
        };
        *remaining_quantum_ns = remaining_quantum_ns.saturating_sub(tick_ns);
        *remaining_quantum_ns == 0
    }

    /// Starts a fresh round-robin quantum after periodic-tick expiration.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FairMode, Nice};

    #[test]
    fn fair_service_request_expires_after_cumulative_small_charges() {
        let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 100, 0);

        assert!(!entity.charge(40, 0));
        assert!(!entity.charge(40, 0));
        assert!(entity.charge(20, 0));
    }

    #[test]
    fn round_robin_quantum_is_not_execution_runtime_budget() {
        let mut entity = SchedulingEntity::RoundRobin {
            remaining_quantum_ns: 30,
        };

        assert!(!entity.charge(30, 0, 0));
        assert_eq!(
            entity,
            SchedulingEntity::RoundRobin {
                remaining_quantum_ns: 30,
            }
        );
    }

    #[test]
    fn round_robin_quantum_advances_by_periodic_ticks() {
        let mut entity = SchedulingEntity::RoundRobin {
            remaining_quantum_ns: 25,
        };

        assert!(!entity.advance_round_robin_tick(10));
        assert!(!entity.advance_round_robin_tick(10));
        assert!(entity.advance_round_robin_tick(10));
    }
}
