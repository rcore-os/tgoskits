//! Per-vCPU GICv3 Redistributor state.

mod mmio;

use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

use crate::{
    CpuInterfaceState, GicAffinity, GicV3VcpuWake, GicVcpuId, IntId, InterruptRecord,
    InterruptState, ListRegisterBacking, ListRegisterFailure, ListRegisterState, LpiId,
    PhysicalIrqId, PpiId, Priority, SgiId, SpiId, TriggerMode, VgicError, VgicResult,
    cpu_interface::MAX_LIST_REGISTERS,
};

/// Highest number of entries one [`RefillOutcome`] can report.
///
/// A refill reports at most one entry per list register, so the fixed
/// list-register count bounds both of its output lists.
const MAX_REFILL_OUTCOME: usize = MAX_LIST_REGISTERS;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QueuedDelivery {
    intid: IntId,
    backing: ListRegisterBacking,
    state: InterruptState,
    maintenance_on_eoi: bool,
}

impl QueuedDelivery {
    const fn software(intid: IntId, trigger: TriggerMode) -> Self {
        Self {
            intid,
            backing: ListRegisterBacking::Software,
            state: InterruptState::Pending,
            maintenance_on_eoi: matches!(trigger, TriggerMode::Level),
        }
    }

    const fn software_with_maintenance(intid: IntId, maintenance_on_eoi: bool) -> Self {
        Self {
            intid,
            backing: ListRegisterBacking::Software,
            state: InterruptState::Pending,
            maintenance_on_eoi,
        }
    }

    const fn physical(intid: IntId, physical: PhysicalIrqId) -> Self {
        Self {
            intid,
            backing: ListRegisterBacking::Physical(physical),
            state: InterruptState::Pending,
            maintenance_on_eoi: false,
        }
    }

    const fn from_list_register(entry: ListRegisterState) -> Self {
        Self {
            intid: entry.intid(),
            backing: entry.backing(),
            state: entry.state(),
            maintenance_on_eoi: entry.maintenance_on_eoi(),
        }
    }

    const fn list_register(self, priority: Priority) -> ListRegisterState {
        match self.backing {
            ListRegisterBacking::Software => ListRegisterState::new_software_with_maintenance(
                self.intid,
                priority,
                self.state,
                self.maintenance_on_eoi,
            ),
            ListRegisterBacking::Physical(physical) => {
                ListRegisterState::new_physical(self.intid, priority, self.state, physical)
            }
        }
    }

    pub(crate) const fn intid(self) -> IntId {
        self.intid
    }

    pub(crate) const fn backing(self) -> ListRegisterBacking {
        self.backing
    }

    pub(crate) const fn state(self) -> InterruptState {
        self.state
    }

    pub(crate) const fn maintenance_on_eoi(self) -> bool {
        self.maintenance_on_eoi
    }

    const fn is_pending_non_active(self) -> bool {
        matches!(self.state, InterruptState::Pending)
    }

    const fn is_active(self) -> bool {
        matches!(
            self.state,
            InterruptState::Active | InterruptState::ActivePending
        )
    }

    fn pend(&mut self) {
        self.state = match self.state {
            InterruptState::Inactive => InterruptState::Pending,
            InterruptState::Active => InterruptState::ActivePending,
            state => state,
        };
    }

    pub(crate) fn set_state(&mut self, state: InterruptState) {
        self.state = state;
    }

    fn clear_pending(&mut self) {
        self.state = match self.state {
            InterruptState::Pending => InterruptState::Inactive,
            InterruptState::ActivePending => InterruptState::Active,
            state => state,
        };
    }
}

/// Result of one list-register refill.
///
/// Both lists hold at most one entry per list register, so they are fixed
/// arrays: a CPU-pinned refill neither allocates nor frees its output.
pub(crate) struct RefillOutcome {
    loaded: [Option<IntId>; MAX_REFILL_OUTCOME],
    spilled_pending: [Option<IntId>; MAX_REFILL_OUTCOME],
}

impl RefillOutcome {
    /// Builds an empty outcome for a refill with nothing to rank.
    fn empty() -> Self {
        Self {
            loaded: [None; MAX_REFILL_OUTCOME],
            spilled_pending: [None; MAX_REFILL_OUTCOME],
        }
    }

    /// Iterates the interrupts this refill moved into list registers.
    pub(crate) fn loaded(&self) -> impl Iterator<Item = IntId> + '_ {
        self.loaded.iter().flatten().copied()
    }

    /// Iterates the in-flight interrupts this refill spilled back to software.
    pub(crate) fn spilled_pending(&self) -> impl Iterator<Item = IntId> + '_ {
        self.spilled_pending.iter().flatten().copied()
    }
}

/// Task-side reserved capacity for one LPI-record expansion.
///
/// Built outside the raw guard by the task-side prepare step, so the in-guard
/// install only moves owned values into reserved storage and swaps buffers.
pub(crate) struct LpiCapacity {
    records: Vec<InterruptRecord>,
    queued: VecDeque<QueuedDelivery>,
    priorities: Vec<Priority>,
    candidates: Vec<(QueuedDelivery, Priority, bool)>,
}

/// Growable LPI storage displaced by an in-guard install.
///
/// The install cannot free these buffers while the raw guard is held, so it
/// hands them back for the caller to drop after releasing the guard. It is a
/// plain tuple so the only operation on it is the deferred drop.
pub(crate) type DisplacedLpiStorage = (
    Vec<InterruptRecord>,
    VecDeque<QueuedDelivery>,
    Vec<Priority>,
    Vec<(QueuedDelivery, Priority, bool)>,
);

/// Allocation-free failure of one list-register refill.
///
/// The CPU-pinned refill runs while the canonical raw lock is held, so it
/// reports the interrupt whose priority source failed rather than formatting a
/// diagnostic there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefillFailure {
    /// The priority of a queued delivery could not be resolved.
    PriorityUnavailable {
        /// Queued interrupt whose priority source rejected the lookup.
        intid: IntId,
    },
    /// No preallocated delivery slot was available.
    QueueFull {
        /// Target vCPU.
        vcpu: GicVcpuId,
        /// Interrupt that could not be queued.
        intid: IntId,
    },
}

impl RefillFailure {
    /// Formats this failure for a task-side caller.
    pub(crate) fn into_vgic_error(self) -> VgicError {
        match self {
            Self::PriorityUnavailable { intid } => VgicError::NativeState {
                operation: "resolve interrupt priority",
                vcpu: None,
                intid: Some(intid),
                reason: "the interrupt has no resolvable priority",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            },
            Self::QueueFull { vcpu, intid } => VgicError::DeliveryQueueFull {
                vcpu: vcpu.raw(),
                intid,
            },
        }
    }
}

impl From<RefillFailure> for VgicError {
    fn from(failure: RefillFailure) -> Self {
        failure.into_vgic_error()
    }
}

pub(crate) struct RedistributorState {
    vcpu: GicVcpuId,
    affinity: GicAffinity,
    private_interrupts: Vec<InterruptRecord>,
    sgi_sources: [u8; 16],
    /// LPI records, kept sorted by raw INTID.
    ///
    /// LPIs are materialized in task context through [`Self::try_install_lpis`]
    /// before they can be delivered, so the native raw paths only look up
    /// existing records and never grow this vector. Growing it in place would
    /// allocate under the raw lock, so the task side reserves capacity outside
    /// the guard and the in-guard install only moves owned values.
    lpis: Vec<InterruptRecord>,
    queued_deliveries: VecDeque<QueuedDelivery>,
    physical_delivery_reserve: usize,
    /// Reusable ranking scratch for one list-register refill.
    ///
    /// Both buffers are reserved at creation from immutable configuration and
    /// only borrowed by value (`core::mem::take`) inside
    /// [`Self::refill_list_registers`], so the CPU-pinned loaded path copies
    /// identities instead of allocating.
    refill_priorities: Vec<Priority>,
    refill_candidates: Vec<(QueuedDelivery, Priority, bool)>,
    /// Reusable scratch for one CPU-interface spill.
    spill_deliveries: Vec<QueuedDelivery>,
    cpu_interface: CpuInterfaceState,
    wake: Arc<dyn GicV3VcpuWake>,
    lpis_enabled: bool,
    propbaser: u64,
    pendbaser: u64,
}

impl RedistributorState {
    pub(crate) fn new(
        vcpu: GicVcpuId,
        affinity: GicAffinity,
        list_register_count: usize,
        spi_count: usize,
        wake: Arc<dyn GicV3VcpuWake>,
    ) -> VgicResult<Self> {
        let mut private_interrupts = Vec::with_capacity(32);
        for raw in 0..32u32 {
            let intid = IntId::new(raw)?;
            let trigger = if raw < 16 {
                TriggerMode::Edge
            } else {
                TriggerMode::Level
            };
            private_interrupts.push(InterruptRecord::new(intid, trigger));
        }
        // Reserve the fixed identity space: `32` private interrupt IDs plus the
        // configured software and hardware-backed SPIs, with one spare slot so
        // the reserve check can never reject the last legal push. LPI records
        // are materialized in task context, and each one grows this queue by a
        // slot through [`Self::try_install_lpis`], so nothing below may grow
        // the queue lazily: every hard-IRQ and loaded-run push either fits or
        // reports `DeliveryQueueFull`.
        let physical_delivery_reserve = spi_count;
        let queue_capacity = 32usize
            .saturating_add(2usize.saturating_mul(spi_count))
            .saturating_add(1);
        Ok(Self {
            vcpu,
            affinity,
            private_interrupts,
            sgi_sources: [0; 16],
            lpis: Vec::new(),
            queued_deliveries: VecDeque::with_capacity(queue_capacity),
            physical_delivery_reserve,
            refill_priorities: Vec::with_capacity(queue_capacity),
            refill_candidates: Vec::with_capacity(queue_capacity + MAX_LIST_REGISTERS),
            spill_deliveries: Vec::with_capacity(MAX_LIST_REGISTERS),
            cpu_interface: CpuInterfaceState::new(list_register_count),
            wake,
            lpis_enabled: false,
            propbaser: 0,
            pendbaser: 0,
        })
    }

    pub(crate) const fn affinity(&self) -> GicAffinity {
        self.affinity
    }

    pub(crate) fn wake(&self) -> Arc<dyn GicV3VcpuWake> {
        self.wake.clone()
    }

    pub(crate) fn private(&self, intid: IntId) -> VgicResult<&InterruptRecord> {
        let raw = intid.raw();
        if raw >= 32 {
            return Err(VgicError::WrongIntIdClass {
                intid,
                operation: "access Redistributor private interrupt",
            });
        }
        Ok(&self.private_interrupts[raw as usize])
    }

    pub(crate) fn private_mut(&mut self, intid: IntId) -> VgicResult<&mut InterruptRecord> {
        let raw = intid.raw();
        if raw >= 32 {
            return Err(VgicError::WrongIntIdClass {
                intid,
                operation: "access Redistributor private interrupt",
            });
        }
        Ok(&mut self.private_interrupts[raw as usize])
    }

    /// Materialized LPI-record count.
    pub(crate) fn lpi_record_count(&self) -> usize {
        self.lpis.len()
    }

    /// Current delivery-queue capacity.
    pub(crate) fn delivery_queue_capacity(&self) -> usize {
        self.queued_deliveries.capacity()
    }

    /// Whether the LPI record is already materialized.
    pub(crate) fn lpi_prepared(&self, lpi: LpiId) -> bool {
        self.lpi_position(lpi).is_ok()
    }

    /// Sorted position of one LPI record, or the position it would insert at.
    fn lpi_position(&self, lpi: LpiId) -> Result<usize, usize> {
        self.lpis
            .binary_search_by_key(&lpi.raw(), |record| record.intid().raw())
    }

    /// Looks up an already-materialized LPI record.
    ///
    /// LPIs are materialized in task context before they can be delivered, so
    /// this returns `None` only for an LPI that was never prepared.
    pub(crate) fn lpi(&self, lpi: LpiId) -> Option<&InterruptRecord> {
        self.lpi_position(lpi).ok().map(|index| &self.lpis[index])
    }

    /// Mutably looks up an already-materialized LPI record.
    pub(crate) fn lpi_mut(&mut self, lpi: LpiId) -> Option<&mut InterruptRecord> {
        let index = self.lpi_position(lpi).ok()?;
        Some(&mut self.lpis[index])
    }

    /// Reserves capacity for `additional` more materialized LPI records and
    /// their delivery slots.
    ///
    /// Allocates, so this is task context only: the caller must not hold the
    /// raw guard. `record_count` and `queue_capacity` are the owning
    /// Redistributor's current values, read under a short raw guard.
    pub(crate) fn reserve_lpi_capacity(
        record_count: usize,
        queue_capacity: usize,
        additional: usize,
    ) -> LpiCapacity {
        let queue = queue_capacity.saturating_add(additional);
        LpiCapacity {
            records: Vec::with_capacity(record_count.saturating_add(additional)),
            queued: VecDeque::with_capacity(queue),
            priorities: Vec::with_capacity(queue),
            candidates: Vec::with_capacity(queue.saturating_add(MAX_LIST_REGISTERS)),
        }
    }

    /// Installs missing LPI records using capacity reserved outside the guard.
    ///
    /// Runs under the raw guard without allocating: it moves the current
    /// growable buffers into the reserved storage and swaps them in, inserting
    /// the missing records in sorted order. The displaced buffers are returned
    /// so the caller drops them after releasing the guard. `None` reports that
    /// a concurrent expansion outgrew the reservation, so the caller
    /// re-reserves and retries.
    pub(crate) fn try_install_lpis(
        &mut self,
        capacity: &mut LpiCapacity,
        new_lpis: &[LpiId],
    ) -> Option<DisplacedLpiStorage> {
        let missing = new_lpis
            .iter()
            .filter(|lpi| self.lpi_position(**lpi).is_err())
            .count();
        if capacity.records.capacity() < self.lpis.len().saturating_add(missing)
            || capacity.queued.capacity()
                < self.queued_deliveries.capacity().saturating_add(missing)
        {
            return None;
        }
        // Move the current record and delivery storage into the reserved
        // buffers. `drain` keeps the displaced buffers allocated so they can be
        // dropped after the raw guard is released.
        let mut old_records = core::mem::take(&mut self.lpis);
        self.lpis = core::mem::take(&mut capacity.records);
        self.lpis.append(&mut old_records);
        let mut old_queued = core::mem::take(&mut self.queued_deliveries);
        self.queued_deliveries = core::mem::take(&mut capacity.queued);
        self.queued_deliveries.extend(old_queued.drain(..));
        let old_priorities = core::mem::replace(
            &mut self.refill_priorities,
            core::mem::take(&mut capacity.priorities),
        );
        let old_candidates = core::mem::replace(
            &mut self.refill_candidates,
            core::mem::take(&mut capacity.candidates),
        );
        let lpis_enabled = self.lpis_enabled;
        for lpi in new_lpis {
            let Err(position) = self.lpi_position(*lpi) else {
                continue;
            };
            let mut record = InterruptRecord::new(IntId::Lpi(*lpi), TriggerMode::Edge);
            record.set_enabled(lpis_enabled);
            self.lpis.insert(position, record);
        }
        Some((old_records, old_queued, old_priorities, old_candidates))
    }

    pub(crate) fn queue(
        &mut self,
        intid: IntId,
        trigger: TriggerMode,
    ) -> Result<(), RefillFailure> {
        self.queue_delivery(QueuedDelivery::software(intid, trigger))
    }

    pub(crate) fn requeue_software(
        &mut self,
        intid: IntId,
        maintenance_on_eoi: bool,
    ) -> Result<(), RefillFailure> {
        self.queue_delivery(QueuedDelivery::software_with_maintenance(
            intid,
            maintenance_on_eoi,
        ))
    }

    /// Queues a hardware-backed delivery.
    ///
    /// Returns `true` only when this acknowledgement reserves a new delivery
    /// slot. A matching queued or loaded LR remains the canonical owner, so
    /// callers must retain any replacement acknowledgement until that stale
    /// delivery is harvested.
    pub(crate) fn queue_physical(
        &mut self,
        intid: IntId,
        physical: PhysicalIrqId,
    ) -> VgicResult<bool> {
        let delivery = QueuedDelivery::physical(intid, physical);
        if prepare_queued_delivery(
            &mut self.queued_deliveries,
            &mut self.cpu_interface,
            delivery,
        ) {
            if self.queued_deliveries.len() >= self.queued_deliveries.capacity() {
                return Err(VgicError::DeliveryQueueFull {
                    vcpu: self.vcpu.raw(),
                    intid,
                });
            }
            self.queued_deliveries.push_back(delivery);
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn has_physical_delivery(&self, intid: IntId, physical: PhysicalIrqId) -> bool {
        let matches = |candidate_intid, backing| {
            candidate_intid == intid && backing == ListRegisterBacking::Physical(physical)
        };
        self.queued_deliveries
            .iter()
            .any(|delivery| matches(delivery.intid(), delivery.backing()))
            || self
                .cpu_interface
                .list_registers()
                .iter()
                .flatten()
                .any(|entry| matches(entry.intid(), entry.backing()))
    }

    pub(crate) fn remove_physical_delivery(&mut self, intid: IntId, physical: PhysicalIrqId) {
        let matches = |candidate_intid, backing| {
            candidate_intid == intid && backing == ListRegisterBacking::Physical(physical)
        };
        self.queued_deliveries
            .retain(|delivery| !matches(delivery.intid(), delivery.backing()));
        for slot in self.cpu_interface.list_registers_mut() {
            if slot
                .as_ref()
                .is_some_and(|entry| matches(entry.intid(), entry.backing()))
            {
                *slot = None;
            }
        }
        self.configure_delivery_traps();
    }

    /// Stages one software-owned delivery without ever growing the queue.
    ///
    /// The capacity was fixed at creation, so the push either fits in
    /// preallocated storage or reports [`VgicError::DeliveryQueueFull`]. The
    /// slots kept free by `physical_delivery_reserve` stay available for
    /// hardware-backed deliveries, so a software flood cannot starve an
    /// assigned physical SPI.
    fn queue_delivery(&mut self, delivery: QueuedDelivery) -> Result<(), RefillFailure> {
        stage_delivery(
            &mut self.queued_deliveries,
            &mut self.cpu_interface,
            self.vcpu,
            self.physical_delivery_reserve,
            delivery,
        )
    }

    pub(crate) fn withdraw_pending_delivery(&mut self, intid: IntId, loaded: bool) -> bool {
        self.clear_queued_pending(intid);
        self.cpu_interface.withdraw_pending_delivery(intid, loaded)
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.queued_deliveries
            .iter()
            .filter(|delivery| delivery.is_pending_non_active())
            .count()
    }

    pub(crate) fn has_pending_delivery(&self) -> bool {
        self.queued_deliveries.iter().any(|delivery| {
            matches!(
                delivery.state(),
                InterruptState::Pending | InterruptState::ActivePending
            )
        }) || self
            .cpu_interface
            .list_registers()
            .iter()
            .flatten()
            .any(|entry| {
                matches!(
                    entry.state(),
                    InterruptState::Pending | InterruptState::ActivePending
                )
            })
    }

    pub(crate) fn cpu_interface(&self) -> &CpuInterfaceState {
        &self.cpu_interface
    }

    pub(crate) fn cpu_interface_mut(&mut self) -> &mut CpuInterfaceState {
        &mut self.cpu_interface
    }

    pub(crate) fn replace_cpu_interface(&mut self, state: CpuInterfaceState) {
        self.cpu_interface = state;
    }

    pub(crate) fn update_list_register_state(
        &mut self,
        index: usize,
        intid: IntId,
        state: InterruptState,
    ) -> Result<(), ListRegisterFailure> {
        let Some(slot) = self.cpu_interface.list_registers_mut().get_mut(index) else {
            return Err(ListRegisterFailure::IndexOutOfRange {
                index,
                intid,
                operation: "synchronize CPU interface",
            });
        };
        let entry = slot.as_mut().filter(|entry| entry.intid() == intid).ok_or(
            ListRegisterFailure::IntIdMismatch {
                index,
                intid,
                operation: "synchronize CPU interface",
            },
        )?;
        entry.set_state(state);
        Ok(())
    }

    pub(crate) fn refill_list_registers(
        &mut self,
        spi_priority: impl FnMut(SpiId) -> VgicResult<Priority>,
    ) -> Result<RefillOutcome, RefillFailure> {
        // Both ranking buffers are reserved at creation; taking them by value
        // leaves an empty placeholder that the restore below replaces with the
        // same allocation, so even the failure path keeps the reserve.
        let mut priorities = core::mem::take(&mut self.refill_priorities);
        let mut candidates = core::mem::take(&mut self.refill_candidates);
        let outcome = self.refill_with_scratch(&mut priorities, &mut candidates, spi_priority);
        self.refill_priorities = priorities;
        self.refill_candidates = candidates;
        outcome
    }

    fn refill_with_scratch(
        &mut self,
        queued_priorities: &mut Vec<Priority>,
        candidates: &mut Vec<(QueuedDelivery, Priority, bool)>,
        mut spi_priority: impl FnMut(SpiId) -> VgicResult<Priority>,
    ) -> Result<RefillOutcome, RefillFailure> {
        if self.queued_deliveries.is_empty() {
            // Existing LR deliveries are already marked in flight. With no
            // software overflow there is nothing to rank, move, or re-mark.
            self.configure_delivery_traps();
            return Ok(RefillOutcome::empty());
        }
        let lr_count = self.cpu_interface.list_registers().len();
        queued_priorities.clear();
        queued_priorities.reserve(self.queued_deliveries.len());
        for delivery in &self.queued_deliveries {
            let intid = delivery.intid();
            let priority = self
                .delivery_priority(intid, &mut spi_priority)
                .map_err(|_| RefillFailure::PriorityUnavailable { intid })?;
            queued_priorities.push(priority);
        }
        candidates.clear();
        candidates.reserve(lr_count + self.queued_deliveries.len());
        for slot in self.cpu_interface.list_registers_mut() {
            let Some(entry) = slot.take() else {
                continue;
            };
            candidates.push((
                QueuedDelivery::from_list_register(entry),
                entry.priority(),
                true,
            ));
        }
        for (delivery, priority) in self.queued_deliveries.drain(..).zip(queued_priorities) {
            candidates.push((delivery, *priority, false));
        }
        candidates.sort_by_key(|(delivery, priority, _)| {
            (
                !delivery.is_pending_non_active(),
                *priority,
                !matches!(delivery.backing(), ListRegisterBacking::Physical(_)),
            )
        });

        let mut outcome = RefillOutcome::empty();
        let mut loaded = 0usize;
        let mut spilled = 0usize;
        for (index, (delivery, priority, was_in_lr)) in candidates.drain(..).enumerate() {
            if index >= lr_count {
                if was_in_lr
                    && delivery.state() == InterruptState::Pending
                    && let Some(slot) = outcome.spilled_pending.get_mut(spilled)
                {
                    *slot = Some(delivery.intid());
                    spilled += 1;
                }
                self.queued_deliveries.push_back(delivery);
                continue;
            }
            let slot = &mut self.cpu_interface.list_registers_mut()[index];
            *slot = Some(delivery.list_register(priority));
            if let Some(slot) = outcome.loaded.get_mut(loaded) {
                *slot = Some(delivery.intid());
                loaded += 1;
            }
        }
        self.configure_delivery_traps();
        Ok(outcome)
    }

    /// Spills every list register back to software delivery state.
    ///
    /// The spill buffer and the reported identity list are both preallocated or
    /// fixed, so the rollback path neither allocates nor frees heap storage.
    /// The spilled identities are reported even when restaging one of them
    /// fails, so the caller can still clear its in-flight mark.
    pub(crate) fn spill_cpu_interface(
        &mut self,
    ) -> ([Option<IntId>; MAX_LIST_REGISTERS], Option<RefillFailure>) {
        let mut deliveries = core::mem::take(&mut self.spill_deliveries);
        deliveries.clear();
        deliveries.extend(
            self.cpu_interface
                .list_registers_mut()
                .iter_mut()
                .filter_map(Option::take)
                .map(QueuedDelivery::from_list_register),
        );
        let mut intids = [None; MAX_LIST_REGISTERS];
        for (slot, delivery) in intids.iter_mut().zip(deliveries.iter()) {
            *slot = Some(delivery.intid());
        }
        let mut failure = None;
        for delivery in deliveries.drain(..) {
            if let Err(error) = self.queue_delivery(delivery)
                && failure.is_none()
            {
                failure = Some(error);
            }
        }
        self.spill_deliveries = deliveries;
        self.configure_delivery_traps();
        (intids, failure)
    }

    pub(crate) fn take_eoi_count(&mut self) -> usize {
        self.cpu_interface.take_eoi_count()
    }

    pub(crate) fn take_active_delivery(&mut self, intid: IntId) -> Option<QueuedDelivery> {
        for slot in self.cpu_interface.list_registers_mut() {
            if slot
                .as_ref()
                .is_some_and(|entry| entry.intid() == intid && is_active(entry.state()))
            {
                return slot.take().map(QueuedDelivery::from_list_register);
            }
        }
        let index = self
            .queued_deliveries
            .iter()
            .position(|delivery| delivery.intid() == intid && delivery.is_active())?;
        self.queued_deliveries.remove(index)
    }

    pub(crate) fn take_next_active_outside(&mut self) -> Option<QueuedDelivery> {
        let index = self
            .queued_deliveries
            .iter()
            .position(|delivery| delivery.is_active())?;
        self.queued_deliveries.remove(index)
    }

    pub(crate) fn highest_pending(
        &self,
        priority_mask: Priority,
        mut spi_priority: impl FnMut(SpiId) -> VgicResult<Priority>,
    ) -> Result<Option<(IntId, Priority)>, RefillFailure> {
        let mut selected = None;
        for delivery in self
            .queued_deliveries
            .iter()
            .filter(|delivery| delivery.is_pending_non_active())
        {
            let intid = delivery.intid();
            let priority = self
                .delivery_priority(intid, &mut spi_priority)
                .map_err(|_| RefillFailure::PriorityUnavailable { intid })?;
            select_pending(&mut selected, intid, priority, priority_mask);
        }
        for entry in self
            .cpu_interface
            .list_registers()
            .iter()
            .flatten()
            .filter(|entry| entry.state() == InterruptState::Pending)
        {
            select_pending(
                &mut selected,
                entry.intid(),
                entry.priority(),
                priority_mask,
            );
        }
        Ok(selected)
    }

    pub(crate) fn take_pending_delivery(&mut self, intid: IntId) -> Option<QueuedDelivery> {
        if let Some(index) = self
            .queued_deliveries
            .iter()
            .position(|delivery| delivery.intid() == intid && delivery.is_pending_non_active())
        {
            let delivery = self.queued_deliveries.remove(index);
            self.configure_delivery_traps();
            return delivery;
        }
        let index = self
            .cpu_interface
            .list_registers()
            .iter()
            .position(|entry| {
                entry.is_some_and(|entry| {
                    entry.intid() == intid && entry.state() == InterruptState::Pending
                })
            })?;
        let delivery = self.cpu_interface.list_registers_mut()[index]
            .take()
            .map(QueuedDelivery::from_list_register);
        self.configure_delivery_traps();
        delivery
    }

    pub(crate) fn store_active_delivery(
        &mut self,
        mut delivery: QueuedDelivery,
        state: InterruptState,
    ) -> Result<(), RefillFailure> {
        delivery.set_state(state);
        self.queue_delivery(delivery)
    }

    fn clear_queued_pending(&mut self, intid: IntId) {
        for delivery in &mut self.queued_deliveries {
            if delivery.intid() == intid {
                delivery.clear_pending();
            }
        }
        self.queued_deliveries
            .retain(|delivery| delivery.state() != InterruptState::Inactive);
    }

    fn delivery_priority(
        &self,
        intid: IntId,
        spi_priority: &mut impl FnMut(SpiId) -> VgicResult<Priority>,
    ) -> VgicResult<Priority> {
        match intid {
            IntId::Sgi(_) | IntId::Ppi(_) => {
                Ok(self.private_interrupts[intid.raw() as usize].priority())
            }
            IntId::Lpi(lpi) => Ok(self
                .lpi(lpi)
                .map_or(Priority::DEFAULT, InterruptRecord::priority)),
            IntId::Spi(spi) => spi_priority(spi),
        }
    }

    fn configure_delivery_traps(&mut self) {
        let pending_outside_lrs = self
            .queued_deliveries
            .iter()
            .any(|delivery| delivery.is_pending_non_active());
        let active_outside_lrs = self
            .queued_deliveries
            .iter()
            .any(|delivery| delivery.is_active());
        let active_in_lrs = self
            .cpu_interface
            .list_registers()
            .iter()
            .flatten()
            .any(|entry| {
                matches!(
                    entry.state(),
                    InterruptState::Active | InterruptState::ActivePending
                )
            });
        self.cpu_interface.configure_delivery_traps(
            pending_outside_lrs,
            active_outside_lrs,
            active_outside_lrs || active_in_lrs,
        );
    }

    pub(crate) fn set_ppi_level(&mut self, ppi: PpiId, asserted: bool, cpu_interface_loaded: bool) {
        let index = ppi.raw() as usize;
        self.private_interrupts[index].set_level(asserted);
        // While a vCPU is loaded, the hardware LRs own their delivery state.
        // Keep the saved LR identity intact until `save` harvests guest EOI;
        // only the input level may change at this point.
        if !asserted
            && !cpu_interface_loaded
            && self.withdraw_pending_delivery(IntId::Ppi(ppi), false)
        {
            self.private_interrupts[index].cancel_inflight();
        }
    }

    pub(crate) fn set_ppi_trigger(&mut self, ppi: PpiId, trigger: TriggerMode) {
        self.private_interrupts[ppi.raw() as usize].set_trigger(trigger);
    }

    pub(crate) fn pulse_ppi(&mut self, ppi: PpiId) {
        self.private_interrupts[ppi.raw() as usize].pulse();
    }

    pub(crate) fn pend_sgi(&mut self, source: GicVcpuId, sgi: SgiId) {
        if source.raw() < 8 {
            self.sgi_sources[sgi.raw() as usize] |= 1 << source.raw();
        }
        self.private_interrupts[sgi.raw() as usize].pulse();
    }

    pub(crate) fn take_sgi_source(&mut self, sgi: SgiId) -> u8 {
        let sources = &mut self.sgi_sources[sgi.raw() as usize];
        let source = if *sources == 0 {
            0
        } else {
            sources.trailing_zeros() as u8
        };
        *sources &= !(1 << source);
        source
    }

    pub(crate) fn has_sgi_sources(&self, sgi: SgiId) -> bool {
        self.sgi_sources[sgi.raw() as usize] != 0
    }

    pub(crate) fn sgi_sources(&self, sgi: SgiId) -> u8 {
        self.sgi_sources[sgi.raw() as usize]
    }

    pub(crate) fn clear_sgi_sources(
        &mut self,
        sgi: SgiId,
        mask: u8,
        cpu_interface_loaded: bool,
    ) -> bool {
        self.sgi_sources[sgi.raw() as usize] &= !mask;
        let empty = !self.has_sgi_sources(sgi);
        if empty {
            let intid = IntId::Sgi(sgi);
            self.private_interrupts[sgi.raw() as usize].set_pending(false);
            if self.withdraw_pending_delivery(intid, cpu_interface_loaded) {
                self.private_interrupts[sgi.raw() as usize].cancel_inflight();
            }
        }
        empty
    }
}

const fn is_active(state: InterruptState) -> bool {
    matches!(
        state,
        InterruptState::Active | InterruptState::ActivePending
    )
}

/// Updates an existing delivery and returns whether a new queue slot is required.
///
/// This is the field-disjoint core of a staging push, so the MMIO trap paths
/// can call it while they still borrow the interrupt record they are updating.
fn prepare_queued_delivery(
    queued_deliveries: &mut VecDeque<QueuedDelivery>,
    cpu_interface: &mut CpuInterfaceState,
    delivery: QueuedDelivery,
) -> bool {
    if let Some(queued) = queued_deliveries
        .iter_mut()
        .find(|queued| queued.intid == delivery.intid)
    {
        if queued.backing == delivery.backing
            && !matches!(queued.backing, ListRegisterBacking::Physical(_))
        {
            queued.pend();
        }
        return false;
    }
    if let Some(index) = cpu_interface
        .list_registers()
        .iter()
        .position(|entry| entry.is_some_and(|entry| entry.intid() == delivery.intid))
    {
        let entry = cpu_interface.list_registers_mut()[index]
            .as_mut()
            .expect("the matched LR slot must remain occupied");
        if entry.backing() == delivery.backing
            && !matches!(entry.backing(), ListRegisterBacking::Physical(_))
        {
            entry.set_state(match entry.state() {
                InterruptState::Inactive => InterruptState::Pending,
                InterruptState::Active => InterruptState::ActivePending,
                state => state,
            });
            cpu_interface.clear_pending_withdrawal(index);
        }
        return false;
    }
    true
}

/// Stages one software-owned delivery into preallocated queue storage.
///
/// Taking the queue, CPU interface, and immutable reserve as separate field
/// borrows lets the MMIO trap paths stage a delivery while they still own the
/// interrupt record borrow that produced it.
fn stage_delivery(
    queued_deliveries: &mut VecDeque<QueuedDelivery>,
    cpu_interface: &mut CpuInterfaceState,
    vcpu: GicVcpuId,
    physical_delivery_reserve: usize,
    delivery: QueuedDelivery,
) -> Result<(), RefillFailure> {
    if !prepare_queued_delivery(queued_deliveries, cpu_interface, delivery) {
        return Ok(());
    }
    let capacity = queued_deliveries.capacity();
    if queued_deliveries
        .len()
        .saturating_add(physical_delivery_reserve)
        >= capacity
    {
        return Err(RefillFailure::QueueFull {
            vcpu,
            intid: delivery.intid(),
        });
    }
    queued_deliveries.push_back(delivery);
    Ok(())
}

fn select_pending(
    selected: &mut Option<(IntId, Priority)>,
    intid: IntId,
    priority: Priority,
    priority_mask: Priority,
) {
    if priority.raw() >= priority_mask.raw() {
        return;
    }
    if selected.is_none_or(|current| (priority, intid) < (current.1, current.0)) {
        *selected = Some((intid, priority));
    }
}

#[cfg(test)]
mod tests;
