//! Saved GICv3 virtual CPU-interface state.

use alloc::vec;

use crate::{IntId, InterruptState, Priority};

/// One virtual interrupt represented in an ICH list register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListRegisterState {
    intid: IntId,
    priority: Priority,
    state: InterruptState,
}

impl ListRegisterState {
    /// Creates a list-register entry.
    pub const fn new(intid: IntId, priority: Priority, state: InterruptState) -> Self {
        Self {
            intid,
            priority,
            state,
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
