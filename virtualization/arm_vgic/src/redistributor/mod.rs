//! Per-vCPU GICv3 Redistributor state.

mod mmio;

use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    vec::Vec,
};

use crate::{
    CpuInterfaceState, GicAffinity, GicV3VcpuWake, GicVcpuId, IntId, InterruptRecord,
    InterruptState, LpiId, PpiId, Priority, SgiId, SpiId, TriggerMode, VgicError, VgicResult,
};

pub(crate) struct RedistributorState {
    vcpu: GicVcpuId,
    affinity: GicAffinity,
    private_interrupts: Vec<InterruptRecord>,
    lpis: BTreeMap<LpiId, InterruptRecord>,
    software_pending: VecDeque<IntId>,
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
            software_pending: VecDeque::new(),
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
        if !self.software_pending.contains(&intid)
            && !self
                .cpu_interface
                .list_registers()
                .iter()
                .flatten()
                .any(|entry| entry.intid() == intid)
        {
            self.software_pending.push_back(intid);
        }
    }

    pub(crate) fn clear_pending_delivery(&mut self, intid: IntId) -> bool {
        self.software_pending.retain(|queued| *queued != intid);
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
        self.software_pending.retain(|queued| *queued != intid);
        let mut canceled = false;
        for slot in self.cpu_interface.list_registers_mut() {
            if slot.as_ref().is_some_and(|entry| {
                entry.intid() == intid && entry.state() == crate::InterruptState::Pending
            }) {
                *slot = None;
                canceled = true;
            }
        }
        canceled
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.software_pending.len()
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
    ) -> VgicResult<Vec<IntId>> {
        const ICH_HCR_UIE: u64 = 1 << 1;

        let available = self
            .cpu_interface
            .list_registers()
            .iter()
            .filter(|slot| slot.is_none())
            .count();
        let mut pending = Vec::with_capacity(self.software_pending.len());
        for intid in self.software_pending.iter().copied() {
            let priority = self.delivery_priority(intid, &mut spi_priority)?;
            pending.push((intid, priority));
        }
        pending.sort_by_key(|(_, priority)| *priority);
        let remaining = pending.split_off(available.min(pending.len()));
        self.software_pending.clear();
        self.software_pending
            .extend(remaining.into_iter().map(|(intid, _)| intid));

        let mut loaded = Vec::with_capacity(pending.len());
        let mut selected = pending.into_iter();
        for slot in self
            .cpu_interface
            .list_registers_mut()
            .iter_mut()
            .filter(|slot| slot.is_none())
        {
            let Some((intid, priority)) = selected.next() else {
                break;
            };
            *slot = Some(crate::ListRegisterState::new(
                intid,
                priority,
                crate::InterruptState::Pending,
            ));
            loaded.push(intid);
        }
        let hcr = self.cpu_interface.hcr();
        self.cpu_interface
            .set_hcr(if self.software_pending.is_empty() {
                hcr & !ICH_HCR_UIE
            } else {
                hcr | ICH_HCR_UIE
            });
        Ok(loaded)
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
