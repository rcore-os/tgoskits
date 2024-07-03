mod lapic;
pub(crate) mod msr;

#[macro_use]
pub(crate) mod regs;

pub(crate) mod ept;

cfg_if::cfg_if! {
    if #[cfg(feature = "vmx")] {
        mod vmx;
        use vmx as vender;
        pub use vmx::{VmxExitInfo, VmxExitReason, VmxInterruptInfo, VmxIoExitInfo};
    }
}

pub(crate) use vender::has_hardware_support;

pub use lapic::ApicTimer;
pub use regs::GeneralRegisters;
pub use vender::{AxVMVcpu, X64NestedPageTable};

// Following are things for the new, unified code structure.

use crate::{AxVMHal, GuestPhysAddr, HostPhysAddr};
use axerrno::AxResult;

/// The architecture dependent configuration of a `AxArchVCpu`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AxArchVCpuConfig {}

// just a stub here
pub struct AxArchVCpu<H: AxVMHal> {
    _marker: core::marker::PhantomData<H>,
}

impl<H: AxVMHal> AxArchVCpu<H> {
    pub fn new(_config: AxArchVCpuConfig) -> AxResult<Self> {
        Ok(Self {
            _marker: core::marker::PhantomData,
        })
    }

    pub fn set_entry_and_ept(&mut self, entry: GuestPhysAddr, ept: HostPhysAddr) -> AxResult {
        unimplemented!()
    }

    pub fn run(&mut self) -> AxResult<crate::vcpu::AxArchVCpuExitReason> {
        unimplemented!()
    }

    pub fn bind(&mut self) -> AxResult {
        unimplemented!()
    }

    pub fn unbind(&mut self) -> AxResult {
        unimplemented!()
    }
}

pub use vender::ArchPerCpuState as AxArchPerCpuState;
