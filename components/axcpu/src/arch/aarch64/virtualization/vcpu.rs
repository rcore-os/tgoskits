//! Guest register ownership around an assembly-only EL1 entry window.

use super::{Exit, GuestContext, GuestSystemRegisters, VirtualTimerState};
use crate::{VirtAddr, registers::FpState};

/// Immutable interrupt-controller handoff needed before the guest timer stops.
/// The platform retains the controller and its acknowledge/EOI lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostIrqConfig {
    pub(crate) interface: u64,
    pub(crate) base: usize,
}

impl HostIrqConfig {
    /// Uses the GICv3 system-register acknowledge interface.
    pub const fn gicv3() -> Self {
        Self {
            interface: 2,
            base: 0,
        }
    }

    /// Uses the GICv2 memory-mapped CPU interface.
    ///
    /// # Safety
    /// `base` must name a mapped, aligned GICv2 CPU interface owned by the
    /// caller. It must remain valid throughout every entry using this config.
    pub unsafe fn gicv2(base: VirtAddr) -> Self {
        Self {
            interface: 1,
            base: base.as_usize(),
        }
    }
}

#[repr(C)]
#[derive(Debug, Default)]
pub(crate) struct HostContext {
    pub stack: u64,
    pub sp_el0: u64,
    pub tpidr_el0: u64,
    pub irq_interface: u64,
    pub irq_base: usize,
    pub timer_hypervisor_control: u64,
    pub timer_kernel_control: u64,
    pub timer_offset: u64,
    pub timer_compare: u64,
    pub timer_control: u64,
    pub cptr: u64,
}

/// Owned guest machine state; it contains no allocator or VM policy objects.
#[repr(C)]
#[derive(Debug, Default)]
pub struct Vcpu {
    /// General-purpose and exception-return registers.
    pub context: GuestContext,
    pub(crate) host: HostContext,
    /// Guest EL1 and stage-2 control registers.
    pub system: GuestSystemRegisters,
    /// Direct virtual timer registers, transferred only during entry and exit.
    pub timer: VirtualTimerState,
    /// Guest FP/SIMD image shared with native task context code.
    pub fp: FpState,
    pub(crate) host_fp: FpState,
    pub(crate) exit: Exit,
}

impl Vcpu {
    /// Installs the platform-owned acknowledge interface for this guest.
    pub fn set_host_irq_interface(&mut self, config: HostIrqConfig) {
        self.host.irq_interface = config.interface;
        self.host.irq_base = config.base;
    }
}
