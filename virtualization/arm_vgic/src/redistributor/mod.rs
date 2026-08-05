//! Per-vCPU GICv3 Redistributor state.

mod mmio;

use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    vec::Vec,
};

use crate::{
    CpuInterfaceState, GicAffinity, GicV3VcpuWake, GicVcpuId, IntId, InterruptRecord,
    InterruptState, ListRegisterBacking, ListRegisterState, LpiId, PhysicalIrqId, PpiId, Priority,
    SgiId, SpiId, TriggerMode, VgicError, VgicResult,
};

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

pub(crate) struct RefillOutcome {
    pub(crate) loaded: Vec<IntId>,
    pub(crate) spilled_pending: Vec<IntId>,
}

pub(crate) struct RedistributorState {
    vcpu: GicVcpuId,
    affinity: GicAffinity,
    private_interrupts: Vec<InterruptRecord>,
    sgi_sources: [u8; 16],
    lpis: BTreeMap<LpiId, InterruptRecord>,
    queued_deliveries: VecDeque<QueuedDelivery>,
    physical_delivery_reserve: usize,
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
        Ok(Self {
            vcpu,
            affinity,
            private_interrupts,
            sgi_sources: [0; 16],
            lpis: BTreeMap::new(),
            queued_deliveries: VecDeque::with_capacity(32 + spi_count),
            physical_delivery_reserve: spi_count,
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

    pub(crate) fn lpi_mut(&mut self, lpi: LpiId) -> &mut InterruptRecord {
        let lpis_enabled = self.lpis_enabled;
        let record = self
            .lpis
            .entry(lpi)
            .or_insert_with(|| InterruptRecord::new(IntId::Lpi(lpi), TriggerMode::Edge));
        record.set_enabled(lpis_enabled);
        record
    }

    pub(crate) fn lpi(&self, lpi: LpiId) -> Option<&InterruptRecord> {
        self.lpis.get(&lpi)
    }

    pub(crate) fn queue(&mut self, intid: IntId, trigger: TriggerMode) {
        self.queue_delivery(QueuedDelivery::software(intid, trigger));
    }

    pub(crate) fn requeue_software(&mut self, intid: IntId, maintenance_on_eoi: bool) {
        self.queue_delivery(QueuedDelivery::software_with_maintenance(
            intid,
            maintenance_on_eoi,
        ));
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
        if self.prepare_queued_delivery(delivery) {
            if self.queued_deliveries.len() == self.queued_deliveries.capacity() {
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

    fn queue_delivery(&mut self, delivery: QueuedDelivery) {
        if !self.prepare_queued_delivery(delivery) {
            return;
        }
        let free_slots = self
            .queued_deliveries
            .capacity()
            .saturating_sub(self.queued_deliveries.len());
        if free_slots <= self.physical_delivery_reserve {
            self.queued_deliveries
                .reserve(self.physical_delivery_reserve.saturating_add(1));
        }
        self.queued_deliveries.push_back(delivery);
    }

    /// Updates an existing delivery and returns whether a new queue slot is required.
    fn prepare_queued_delivery(&mut self, delivery: QueuedDelivery) -> bool {
        if let Some(queued) = self
            .queued_deliveries
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
        if let Some(entry) = self
            .cpu_interface
            .list_registers_mut()
            .iter_mut()
            .flatten()
            .find(|entry| entry.intid() == delivery.intid)
        {
            if entry.backing() == delivery.backing
                && !matches!(entry.backing(), ListRegisterBacking::Physical(_))
            {
                entry.set_state(match entry.state() {
                    InterruptState::Inactive => InterruptState::Pending,
                    InterruptState::Active => InterruptState::ActivePending,
                    state => state,
                });
            }
            return false;
        }
        true
    }

    pub(crate) fn clear_pending_delivery(&mut self, intid: IntId) -> bool {
        self.clear_queued_pending(intid);
        let mut canceled = false;
        for slot in self.cpu_interface.list_registers_mut() {
            let Some(entry) = slot.as_mut().filter(|entry| entry.intid() == intid) else {
                continue;
            };
            match entry.state() {
                crate::InterruptState::Pending => {
                    *slot = None;
                    canceled = true;
                }
                crate::InterruptState::ActivePending => {
                    entry.set_state(crate::InterruptState::Active);
                }
                crate::InterruptState::Inactive | crate::InterruptState::Active => {}
            }
        }
        canceled
    }

    pub(crate) fn withdraw_pending_delivery(&mut self, intid: IntId) -> bool {
        self.clear_queued_pending(intid);
        let mut canceled = false;
        for slot in self.cpu_interface.list_registers_mut() {
            let Some(entry) = slot.as_mut().filter(|entry| entry.intid() == intid) else {
                continue;
            };
            match entry.state() {
                InterruptState::Pending => {
                    *slot = None;
                    canceled = true;
                }
                InterruptState::ActivePending => entry.set_state(InterruptState::Active),
                InterruptState::Inactive | InterruptState::Active => {}
            }
        }
        canceled
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
    ) -> VgicResult {
        let slot = self
            .cpu_interface
            .list_registers_mut()
            .get_mut(index)
            .ok_or_else(|| VgicError::InvalidStateTransition {
                intid,
                operation: "synchronize CPU interface",
                detail: alloc::format!("list-register index {index} is out of range"),
            })?;
        let entry = slot
            .as_mut()
            .filter(|entry| entry.intid() == intid)
            .ok_or_else(|| VgicError::InvalidStateTransition {
                intid,
                operation: "synchronize CPU interface",
                detail: alloc::format!(
                    "list-register index {index} no longer contains the expected INTID"
                ),
            })?;
        entry.set_state(state);
        Ok(())
    }

    pub(crate) fn refill_list_registers(
        &mut self,
        mut spi_priority: impl FnMut(SpiId) -> VgicResult<Priority>,
    ) -> VgicResult<RefillOutcome> {
        let lr_count = self.cpu_interface.list_registers().len();
        let queued_priorities = self
            .queued_deliveries
            .iter()
            .map(|delivery| self.delivery_priority(delivery.intid(), &mut spi_priority))
            .collect::<VgicResult<Vec<_>>>()?;
        let mut candidates = Vec::with_capacity(lr_count + self.queued_deliveries.len());
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
            candidates.push((delivery, priority, false));
        }
        candidates.sort_by_key(|(delivery, priority, _)| {
            (
                !delivery.is_pending_non_active(),
                *priority,
                !matches!(delivery.backing(), ListRegisterBacking::Physical(_)),
            )
        });

        let mut loaded = Vec::with_capacity(lr_count.min(candidates.len()));
        let mut spilled_pending = Vec::new();
        for (index, (delivery, priority, was_in_lr)) in candidates.into_iter().enumerate() {
            if index >= lr_count {
                if was_in_lr && delivery.state() == InterruptState::Pending {
                    spilled_pending.push(delivery.intid());
                }
                self.queued_deliveries.push_back(delivery);
                continue;
            }
            let slot = &mut self.cpu_interface.list_registers_mut()[index];
            *slot = Some(delivery.list_register(priority));
            loaded.push(delivery.intid);
        }
        self.configure_delivery_traps();
        Ok(RefillOutcome {
            loaded,
            spilled_pending,
        })
    }

    pub(crate) fn spill_cpu_interface(&mut self) -> Vec<IntId> {
        let deliveries = self
            .cpu_interface
            .list_registers_mut()
            .iter_mut()
            .filter_map(Option::take)
            .map(QueuedDelivery::from_list_register)
            .collect::<Vec<_>>();
        let intids = deliveries.iter().map(|delivery| delivery.intid()).collect();
        for delivery in deliveries {
            self.queue_delivery(delivery);
        }
        self.configure_delivery_traps();
        intids
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
    ) -> VgicResult<Option<(IntId, Priority)>> {
        let mut selected = None;
        for delivery in self
            .queued_deliveries
            .iter()
            .filter(|delivery| delivery.is_pending_non_active())
        {
            let priority = self.delivery_priority(delivery.intid(), &mut spi_priority)?;
            select_pending(&mut selected, delivery.intid(), priority, priority_mask);
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
    ) {
        delivery.set_state(state);
        self.queue_delivery(delivery);
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
                .lpis
                .get(&lpi)
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
        if !asserted && !cpu_interface_loaded && self.clear_pending_delivery(IntId::Ppi(ppi)) {
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

    pub(crate) fn clear_sgi_sources(&mut self, sgi: SgiId, mask: u8) -> bool {
        self.sgi_sources[sgi.raw() as usize] &= !mask;
        let empty = !self.has_sgi_sources(sgi);
        if empty {
            let intid = IntId::Sgi(sgi);
            self.private_interrupts[sgi.raw() as usize].set_pending(false);
            if self.clear_pending_delivery(intid) {
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
