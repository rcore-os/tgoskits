//! Saved GICv3 virtual CPU-interface state.

use alloc::vec;

use crate::{IntId, InterruptState, PhysicalIrqId, Priority};

const ICH_HCR_ENABLE: u64 = 1;
const ICH_HCR_UIE: u64 = 1 << 1;
const ICH_HCR_LRENPIE: u64 = 1 << 2;
const ICH_HCR_NPIE: u64 = 1 << 3;
const ICH_HCR_TDIR: u64 = 1 << 14;
const ICH_HCR_EOI_COUNT_SHIFT: u32 = 27;
const ICH_HCR_EOI_COUNT_MASK: u64 = 0x1f << ICH_HCR_EOI_COUNT_SHIFT;

/// Source backing used for one virtual list-register delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListRegisterBacking {
    /// The hypervisor owns the complete virtual interrupt lifecycle.
    Software,
    /// The physical GIC owns pending/active state and the LR names its source.
    Physical(PhysicalIrqId),
}

/// One virtual interrupt represented in an ICH list register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListRegisterState {
    intid: IntId,
    priority: Priority,
    state: InterruptState,
    backing: ListRegisterBacking,
}

impl ListRegisterState {
    /// Creates a list-register entry.
    pub const fn new(intid: IntId, priority: Priority, state: InterruptState) -> Self {
        Self {
            intid,
            priority,
            state,
            backing: ListRegisterBacking::Software,
        }
    }

    /// Creates an entry backed by one ownership-checked physical interrupt.
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

    /// Updates the saved delivery state.
    pub fn set_state(&mut self, state: InterruptState) {
        self.state = state;
    }
}

/// Complete ICH state saved for one vCPU.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuInterfaceState {
    hcr: u64,
    vmcr: u64,
    apr: [u64; 4],
    list_registers: alloc::vec::Vec<Option<ListRegisterState>>,
}

impl CpuInterfaceState {
    pub(crate) fn new(list_register_count: usize) -> Self {
        Self {
            hcr: 1,
            vmcr: 0,
            apr: [0; 4],
            list_registers: vec![None; list_register_count],
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
}
