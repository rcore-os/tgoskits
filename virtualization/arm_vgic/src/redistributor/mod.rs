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
}

impl QueuedDelivery {
    const fn software(intid: IntId) -> Self {
        Self {
            intid,
            backing: ListRegisterBacking::Software,
            state: InterruptState::Pending,
        }
    }

    const fn physical(intid: IntId, physical: PhysicalIrqId) -> Self {
        Self {
            intid,
            backing: ListRegisterBacking::Physical(physical),
            state: InterruptState::Pending,
        }
    }

    const fn from_list_register(entry: ListRegisterState) -> Self {
        Self {
            intid: entry.intid(),
            backing: entry.backing(),
            state: entry.state(),
        }
    }

    const fn list_register(self, priority: Priority) -> ListRegisterState {
        match self.backing {
            ListRegisterBacking::Software => {
                ListRegisterState::new(self.intid, priority, self.state)
            }
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

    const fn is_pending(self) -> bool {
        matches!(
            self.state,
            InterruptState::Pending | InterruptState::ActivePending
        )
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
    lpis: BTreeMap<LpiId, InterruptRecord>,
    queued_deliveries: VecDeque<QueuedDelivery>,
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
            lpis: BTreeMap::new(),
            queued_deliveries: VecDeque::new(),
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

    pub(crate) fn queue(&mut self, intid: IntId) {
        self.queue_delivery(QueuedDelivery::software(intid));
    }

    pub(crate) fn queue_physical(&mut self, intid: IntId, physical: PhysicalIrqId) {
        self.queue_delivery(QueuedDelivery::physical(intid, physical));
    }

    fn queue_delivery(&mut self, delivery: QueuedDelivery) {
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
            return;
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
            return;
        }
        self.queued_deliveries.push_back(delivery);
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
            .filter(|delivery| delivery.is_pending())
            .count()
    }

    pub(crate) fn cpu_interface(&self) -> &CpuInterfaceState {
        &self.cpu_interface
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
        while let Some(delivery) = self.queued_deliveries.pop_front() {
            let priority = self.delivery_priority(delivery.intid, &mut spi_priority)?;
            candidates.push((delivery, priority, false));
        }
        candidates.sort_by_key(|(delivery, priority, _)| {
            (
                !delivery.is_pending(),
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
        let pending_outside_lrs = self
            .queued_deliveries
            .iter()
            .any(|delivery| delivery.is_pending());
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
        Ok(RefillOutcome {
            loaded,
            spilled_pending,
        })
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

    fn clear_queued_pending(&mut self, intid: IntId) {
        let mut retained = VecDeque::with_capacity(self.queued_deliveries.len());
        while let Some(mut delivery) = self.queued_deliveries.pop_front() {
            if delivery.intid() == intid {
                delivery.clear_pending();
            }
            if delivery.state() != InterruptState::Inactive {
                retained.push_back(delivery);
            }
        }
        self.queued_deliveries = retained;
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

    pub(crate) fn set_ppi_level(&mut self, ppi: PpiId, asserted: bool) {
        self.private_interrupts[ppi.raw() as usize].set_level(asserted);
    }

    pub(crate) fn set_ppi_trigger(&mut self, ppi: PpiId, trigger: TriggerMode) {
        self.private_interrupts[ppi.raw() as usize].set_trigger(trigger);
    }

    pub(crate) fn pulse_ppi(&mut self, ppi: PpiId) {
        self.private_interrupts[ppi.raw() as usize].pulse();
    }

    pub(crate) fn pend_sgi(&mut self, sgi: SgiId) {
        self.private_interrupts[sgi.raw() as usize].pulse();
    }
}

const fn is_active(state: InterruptState) -> bool {
    matches!(
        state,
        InterruptState::Active | InterruptState::ActivePending
    )
}
