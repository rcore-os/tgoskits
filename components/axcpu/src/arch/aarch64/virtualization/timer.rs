//! Direct virtual timer registers transferred at guest entry and exit.

/// Machine image of the EL1 virtual timer, independent of scheduling policy.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VirtualTimerState {
    /// CNTVOFF_EL2, subtracted from the physical counter.
    pub offset: u64,
    /// CNTV_CVAL_EL0, in the virtual counter domain.
    pub compare: u64,
    /// Writable ENABLE and IMASK bits of CNTV_CTL_EL0.
    pub control: u32,
    /// CNTHCTL_EL2 access controls for the guest.
    pub hypervisor_control: u64,
    /// Guest CNTKCTL_EL1 permissions saved across entries.
    pub kernel_control: u64,
}
