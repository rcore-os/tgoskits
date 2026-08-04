//! Saved GICv3 virtual CPU-interface state.

use alloc::vec;

use crate::{IntId, InterruptState, PhysicalIrqId, Priority, TriggerMode};

const ICH_HCR_ENABLE: u64 = 1;
const ICH_HCR_UIE: u64 = 1 << 1;
const ICH_HCR_LRENPIE: u64 = 1 << 2;
const ICH_HCR_NPIE: u64 = 1 << 3;
const ICH_HCR_TDIR: u64 = 1 << 14;
const ICH_HCR_EOI_COUNT_SHIFT: u32 = 27;
const ICH_HCR_EOI_COUNT_MASK: u64 = 0x1f << ICH_HCR_EOI_COUNT_SHIFT;
const ICH_VMCR_VENG1: u64 = 1 << 1;
const ICH_VMCR_VEOIM: u64 = 1 << 9;
const ICH_VMCR_VPMR_SHIFT: u32 = 24;
const ICH_VMCR_VPMR_MASK: u64 = 0xff << ICH_VMCR_VPMR_SHIFT;

/// Source backing used for one virtual list-register delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListRegisterBacking {
    /// The hypervisor owns the complete virtual interrupt lifecycle.
    Software,
    /// The physical GIC owns pending/active state and the LR names its source.
    ///
    /// Guest deactivation can consequently retire the physical activation in
    /// hardware. A trapped DIR still uses this identity to complete the exact
    /// ownership-checked host source.
    Physical(PhysicalIrqId),
}

/// One virtual interrupt represented in an ICH list register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListRegisterState {
    intid: IntId,
    priority: Priority,
    state: InterruptState,
    backing: ListRegisterBacking,
    maintenance_on_eoi: bool,
}

impl ListRegisterState {
    /// Creates a list-register entry.
    pub const fn new(intid: IntId, priority: Priority, state: InterruptState) -> Self {
        Self {
            intid,
            priority,
            state,
            backing: ListRegisterBacking::Software,
            maintenance_on_eoi: false,
        }
    }

    /// Creates a software-backed entry with trigger-aware EOI maintenance.
    pub const fn new_software(
        intid: IntId,
        priority: Priority,
        state: InterruptState,
        trigger: TriggerMode,
    ) -> Self {
        Self::new_software_with_maintenance(
            intid,
            priority,
            state,
            matches!(trigger, TriggerMode::Level),
        )
    }

    pub(crate) const fn new_software_with_maintenance(
        intid: IntId,
        priority: Priority,
        state: InterruptState,
        maintenance_on_eoi: bool,
    ) -> Self {
        Self {
            intid,
            priority,
            state,
            backing: ListRegisterBacking::Software,
            maintenance_on_eoi,
        }
    }

    /// Creates a hardware-backed entry for one ownership-checked physical interrupt.
    pub const fn new_physical(
        intid: IntId,
        priority: Priority,
        state: InterruptState,
        physical: PhysicalIrqId,
    ) -> Self {
        Self {
            intid,
            priority,
            state,
            backing: ListRegisterBacking::Physical(physical),
            maintenance_on_eoi: false,
        }
    }

    /// Returns the represented INTID.
    pub const fn intid(self) -> IntId {
        self.intid
    }

    /// Returns the virtual priority.
    pub const fn priority(self) -> Priority {
        self.priority
    }

    /// Returns the saved delivery state.
    pub const fn state(self) -> InterruptState {
        self.state
    }

    /// Returns whether delivery state is software-owned or physical-GIC-backed.
    pub const fn backing(self) -> ListRegisterBacking {
        self.backing
    }

    /// Returns whether guest deactivation must raise a maintenance interrupt.
    pub const fn maintenance_on_eoi(self) -> bool {
        self.maintenance_on_eoi
    }

    /// Updates the saved delivery state.
    pub fn set_state(&mut self, state: InterruptState) {
        self.state = state;
    }
}

#[cfg(test)]
mod tests {
    use super::ListRegisterState;
    use crate::{IntId, InterruptState, PpiId, Priority, TriggerMode};

    #[test]
    fn only_software_level_delivery_requests_eoi_maintenance() {
        let intid = IntId::Ppi(PpiId::new(27).unwrap());
        let level = ListRegisterState::new_software(
            intid,
            Priority::DEFAULT,
            InterruptState::Pending,
            TriggerMode::Level,
        );
        let edge = ListRegisterState::new_software(
            intid,
            Priority::DEFAULT,
            InterruptState::Pending,
            TriggerMode::Edge,
        );

        assert!(level.maintenance_on_eoi());
        assert!(!edge.maintenance_on_eoi());
    }
}

/// Complete ICH state saved for one vCPU.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuInterfaceState {
    hcr: u64,
    vmcr: u64,
    apr: [u64; 4],
    list_registers: alloc::vec::Vec<Option<ListRegisterState>>,
    v2_enabled: bool,
    v2_priority_mask: Priority,
    v2_binary_point: u8,
    v2_eoi_mode: bool,
    v2_active_stack: alloc::vec::Vec<(IntId, Priority)>,
}

impl CpuInterfaceState {
    pub(crate) fn new(list_register_count: usize) -> Self {
        Self {
            hcr: 1,
            vmcr: ICH_VMCR_VENG1 | ICH_VMCR_VPMR_MASK,
            apr: [0; 4],
            list_registers: vec![None; list_register_count],
            v2_enabled: false,
            v2_priority_mask: Priority::new(0),
            v2_binary_point: 0,
            v2_eoi_mode: false,
            v2_active_stack: alloc::vec::Vec::new(),
        }
    }

    /// Returns ICH_HCR_EL2 state.
    pub const fn hcr(&self) -> u64 {
        self.hcr
    }

    /// Updates ICH_HCR_EL2 state.
    pub fn set_hcr(&mut self, value: u64) {
        self.hcr = value;
    }

    pub(crate) fn take_eoi_count(&mut self) -> usize {
        let count = ((self.hcr & ICH_HCR_EOI_COUNT_MASK) >> ICH_HCR_EOI_COUNT_SHIFT) as usize;
        self.hcr &= !ICH_HCR_EOI_COUNT_MASK;
        count
    }

    pub(crate) fn configure_delivery_traps(
        &mut self,
        pending_outside_lrs: bool,
        active_outside_lrs: bool,
        trap_deactivation: bool,
    ) {
        let managed =
            ICH_HCR_UIE | ICH_HCR_LRENPIE | ICH_HCR_NPIE | ICH_HCR_TDIR | ICH_HCR_EOI_COUNT_MASK;
        let mut hcr = (self.hcr & !managed) | ICH_HCR_ENABLE;
        if pending_outside_lrs || active_outside_lrs {
            hcr |= ICH_HCR_UIE;
        }
        if active_outside_lrs {
            hcr |= ICH_HCR_LRENPIE;
        }
        if pending_outside_lrs {
            hcr |= ICH_HCR_NPIE;
        }
        if trap_deactivation {
            hcr |= ICH_HCR_TDIR;
        }
        self.hcr = hcr;
    }

    /// Returns ICH_VMCR_EL2 state.
    pub const fn vmcr(&self) -> u64 {
        self.vmcr
    }

    /// Updates ICH_VMCR_EL2 state.
    pub fn set_vmcr(&mut self, value: u64) {
        self.vmcr = value;
    }

    /// Returns the guest-visible common ICC control bits.
    pub const fn icc_control(&self) -> u64 {
        ((self.vmcr & ICH_VMCR_VEOIM != 0) as u64) << 1
    }

    /// Updates writable common ICC control bits.
    pub fn set_icc_control(&mut self, value: u64) {
        if value & (1 << 1) != 0 {
            self.vmcr |= ICH_VMCR_VEOIM;
        } else {
            self.vmcr &= !ICH_VMCR_VEOIM;
        }
    }

    /// Returns the guest-visible virtual priority mask.
    pub const fn icc_priority_mask(&self) -> u8 {
        ((self.vmcr & ICH_VMCR_VPMR_MASK) >> ICH_VMCR_VPMR_SHIFT) as u8
    }

    /// Updates the guest-visible virtual priority mask.
    pub fn set_icc_priority_mask(&mut self, value: u8) {
        self.vmcr = (self.vmcr & !ICH_VMCR_VPMR_MASK) | (u64::from(value) << ICH_VMCR_VPMR_SHIFT);
    }

    /// Returns the priority of the highest-priority active LR.
    pub fn icc_running_priority(&self) -> Priority {
        self.list_registers
            .iter()
            .flatten()
            .filter(|entry| {
                matches!(
                    entry.state(),
                    InterruptState::Active | InterruptState::ActivePending
                )
            })
            .map(|entry| entry.priority())
            .min()
            .unwrap_or_else(|| Priority::new(0xff))
    }

    /// Returns saved active-priority registers.
    pub const fn apr(&self) -> &[u64; 4] {
        &self.apr
    }

    /// Updates one active-priority register.
    pub fn set_apr(&mut self, index: usize, value: u64) -> bool {
        if let Some(register) = self.apr.get_mut(index) {
            *register = value;
            true
        } else {
            false
        }
    }

    /// Returns all list-register slots.
    pub fn list_registers(&self) -> &[Option<ListRegisterState>] {
        &self.list_registers
    }

    /// Returns mutable list-register slots for a checked backend save.
    pub fn list_registers_mut(&mut self) -> &mut [Option<ListRegisterState>] {
        &mut self.list_registers
    }

    /// Returns the guest-visible GICC_CTLR state.
    pub fn v2_control(&self) -> u32 {
        self.v2_enabled as u32 | ((self.v2_eoi_mode as u32) << 9)
    }

    pub(crate) fn set_v2_control(&mut self, value: u32) {
        self.v2_enabled = value & 1 != 0;
        self.v2_eoi_mode = value & (1 << 9) != 0;
    }

    /// Returns whether the GICv2 virtual CPU interface is enabled.
    pub const fn v2_enabled(&self) -> bool {
        self.v2_enabled
    }

    /// Returns the GICv2 virtual priority mask.
    pub const fn v2_priority_mask(&self) -> Priority {
        self.v2_priority_mask
    }

    pub(crate) fn set_v2_priority_mask(&mut self, value: u8) {
        self.v2_priority_mask = Priority::new(value);
    }

    /// Returns the GICv2 virtual binary point.
    pub const fn v2_binary_point(&self) -> u8 {
        self.v2_binary_point
    }

    pub(crate) fn set_v2_binary_point(&mut self, value: u8) {
        self.v2_binary_point = value & 0x7;
    }

    /// Returns whether split EOI/deactivation mode is enabled.
    pub const fn v2_eoi_mode(&self) -> bool {
        self.v2_eoi_mode
    }

    pub(crate) fn push_v2_active(&mut self, intid: IntId, priority: Priority) {
        self.v2_active_stack.push((intid, priority));
    }

    pub(crate) fn drop_v2_priority(&mut self, intid: IntId) -> bool {
        if self
            .v2_active_stack
            .last()
            .is_none_or(|(active, _)| *active != intid)
        {
            return false;
        }
        self.v2_active_stack.pop();
        true
    }

    /// Returns the priority of the top GICv2 active interrupt.
    pub fn v2_running_priority(&self) -> Priority {
        self.v2_active_stack
            .last()
            .map_or(Priority::new(0xff), |(_, priority)| *priority)
    }
}
