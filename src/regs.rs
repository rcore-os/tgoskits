#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct GeneralPurposeRegisters([usize; 32]);

/// Index of risc-v general purpose registers in `GeneralPurposeRegisters`.
#[allow(missing_docs)]
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GprIndex {
    Zero = 0,
    RA,
    SP,
    GP,
    TP,
    T0,
    T1,
    T2,
    S0,
    S1,
    A0,
    A1,
    A2,
    A3,
    A4,
    A5,
    A6,
    A7,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S10,
    S11,
    T3,
    T4,
    T5,
    T6,
}

impl GprIndex {
    /// Get register index from raw value.
    pub fn from_raw(raw: u32) -> Option<Self> {
        use GprIndex::*;
        let index = match raw {
            0 => Zero,
            1 => RA,
            2 => SP,
            3 => GP,
            4 => TP,
            5 => T0,
            6 => T1,
            7 => T2,
            8 => S0,
            9 => S1,
            10 => A0,
            11 => A1,
            12 => A2,
            13 => A3,
            14 => A4,
            15 => A5,
            16 => A6,
            17 => A7,
            18 => S2,
            19 => S3,
            20 => S4,
            21 => S5,
            22 => S6,
            23 => S7,
            24 => S8,
            25 => S9,
            26 => S10,
            27 => S11,
            28 => T3,
            29 => T4,
            30 => T5,
            31 => T6,
            _ => {
                return None;
            }
        };
        Some(index)
    }
}

impl GeneralPurposeRegisters {
    /// Returns the value of the given register.
    pub fn reg(&self, reg_index: GprIndex) -> usize {
        self.0[reg_index as usize]
    }

    /// Sets the value of the given register.
    pub fn set_reg(&mut self, reg_index: GprIndex, val: usize) {
        if reg_index == GprIndex::Zero {
            return;
        }

        self.0[reg_index as usize] = val;
    }

    /// Returns the argument registers.
    /// This is avoids many calls when an SBI handler needs all of the argmuent regs.
    pub fn a_regs(&self) -> &[usize] {
        &self.0[GprIndex::A0 as usize..=GprIndex::A7 as usize]
    }

    /// Returns the arguments register as a mutable.
    pub fn a_regs_mut(&mut self) -> &mut [usize] {
        &mut self.0[GprIndex::A0 as usize..=GprIndex::A7 as usize]
    }
}

/// Hypervisor GPR and CSR state which must be saved/restored when entering/exiting virtualization.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct HypervisorCpuState {
    pub gprs: GeneralPurposeRegisters,
    pub sstatus: usize,
    pub hstatus: usize,
    pub scounteren: usize,
    pub stvec: usize,
    pub sscratch: usize,
}

/// Guest GPR and CSR state which must be saved/restored when exiting/entering virtualization.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct GuestCpuState {
    pub gprs: GeneralPurposeRegisters,
    pub sstatus: usize,
    pub hstatus: usize,
    pub scounteren: usize,
    pub sepc: usize,
}

/// The CSRs that are only in effect when virtualization is enabled (V=1) and must be saved and
/// restored whenever we switch between VMs.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct GuestVsCsrs {
    pub htimedelta: usize,
    pub vsstatus: usize,
    pub vsie: usize,
    pub vstvec: usize,
    pub vsscratch: usize,
    pub vsepc: usize,
    pub vscause: usize,
    pub vstval: usize,
    pub vsatp: usize,
    pub vstimecmp: usize,
}

/// Virtualized HS-level CSRs that are used to emulate (part of) the hypervisor extension for the
/// guest.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct GuestVirtualHsCsrs {
    pub hie: usize,
    pub hgeie: usize,
    pub hgatp: usize,
}

/// CSRs written on an exit from virtualization that are used by the hypervisor to determine the cause
/// of the trap.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct VmCpuTrapState {
    pub scause: usize,
    pub stval: usize,
    pub htval: usize,
    pub htinst: usize,
}

/// (v)CPU register state that must be saved or restored when entering/exiting a VM or switching
/// between VMs.
#[derive(Debug, Default, Clone)]
#[repr(C)]
pub struct VmCpuRegisters {
    // CPU state that's shared between our's and the guest's execution environment. Saved/restored
    // when entering/exiting a VM.
    pub hyp_regs: HypervisorCpuState,
    pub guest_regs: GuestCpuState,

    // CPU state that only applies when V=1, e.g. the VS-level CSRs. Saved/restored on activation of
    // the vCPU.
    pub vs_csrs: GuestVsCsrs,

    // Virtualized HS-level CPU state.
    pub virtual_hs_csrs: GuestVirtualHsCsrs,

    // Read on VM exit.
    pub trap_csrs: VmCpuTrapState,
}
