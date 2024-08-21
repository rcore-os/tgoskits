use aarch64_cpu::registers::*;

use axerrno::AxResult;
use axvcpu::AxArchPerCpu;

/// Per-CPU data. A pointer to this struct is loaded into TP when a CPU starts. This structure
#[repr(C)]
#[repr(align(4096))]
pub struct Aarch64PerCpu {
    //stack_top_addr has no use yet?
    /// per cpu id
    pub cpu_id: usize,
    /// context address of this cpu
    pub ctx: Option<usize>,
}

#[percpu::def_percpu]
static ORI_EXCEPTION_VECTOR_BASE: usize = 0;

extern "C" {
    fn exception_vector_base_vcpu();
}

impl AxArchPerCpu for Aarch64PerCpu {
    fn new(cpu_id: usize) -> AxResult<Self> {
        Ok(Self { cpu_id, ctx: None })
    }

    fn is_enabled(&self) -> bool {
        let hcr_el2 = HCR_EL2.get();
        return hcr_el2 & 1 != 0;
    }

    fn hardware_enable(&mut self) -> AxResult {
        // First we save origin `exception_vector_base`.
        // Safety:
        // Todo: take care of `preemption`
        unsafe { ORI_EXCEPTION_VECTOR_BASE.write_current_raw(VBAR_EL2.get() as usize) }

        // Set current `VBAR_EL2` to `exception_vector_base_vcpu`
        // defined in this crate.
        VBAR_EL2.set(exception_vector_base_vcpu as usize as _);

        Ok(HCR_EL2.set(HCR_EL2::VM::Enable.into()))
    }

    fn hardware_disable(&mut self) -> AxResult {
        // Reset `VBAR_EL2` into previous value.
        // Safety:
        // Todo: take care of `preemption`
        VBAR_EL2.set(unsafe { ORI_EXCEPTION_VECTOR_BASE.read_current_raw() } as _);

        Ok(HCR_EL2.set(HCR_EL2::VM::Disable.into()))
    }
}
