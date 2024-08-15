use aarch64_cpu::registers::*;

use axerrno::AxResult;

use crate::percpu::AxVMArchPerCpu;
use crate::AxVMHal;

/// Per-CPU data. A pointer to this struct is loaded into TP when a CPU starts. This structure
#[repr(C)]
#[repr(align(4096))]
pub struct PerCpu<H: AxVMHal> {
    //stack_top_addr has no use yet?
    /// per cpu id
    pub cpu_id: usize,
    /// context address of this cpu
    pub ctx: Option<usize>,

    marker: core::marker::PhantomData<H>,
}

impl<H: AxVMHal + 'static> AxVMArchPerCpu for PerCpu<H> {
    fn new(cpu_id: usize) -> AxResult<Self> {
        Ok(Self {
            cpu_id: cpu_id,
            ctx: None,

            marker: core::marker::PhantomData,
        })
    }

    fn is_enabled(&self) -> bool {
        let hcr_el2 = HCR_EL2.get();
        return hcr_el2 & 1 != 0;
    }

    fn hardware_enable(&mut self) -> AxResult {
        Ok(HCR_EL2.set(HCR_EL2::VM::Enable.into()))
    }

    fn hardware_disable(&mut self) -> AxResult {
        Ok(HCR_EL2.set(HCR_EL2::VM::Disable.into()))
    }
}
