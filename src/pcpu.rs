use core::{cell::OnceCell, marker::PhantomData};

use aarch64_cpu::registers::*;
use axerrno::AxResult;
use axvcpu::{AxArchPerCpu, AxVCpuHal};
use tock_registers::interfaces::ReadWriteable;

/// Per-CPU data. A pointer to this struct is loaded into TP when a CPU starts. This structure
#[repr(C)]
#[repr(align(4096))]
pub struct Aarch64PerCpu<H: AxVCpuHal> {
    /// per cpu id
    pub cpu_id: usize,
    _phantom: PhantomData<H>,
}

#[percpu::def_percpu]
static ORI_EXCEPTION_VECTOR_BASE: usize = 0;

/// IRQ handler registered by underlying host OS during per-cpu initialization,
/// for dispatching IRQs to the host OS.
///
/// Set `IRQ_HANDLER` as per-cpu variable to avoid the need of `OnceLock`.
#[percpu::def_percpu]
pub static IRQ_HANDLER: OnceCell<&(dyn Fn() + Send + Sync)> = OnceCell::new();

unsafe extern "C" {
    fn exception_vector_base_vcpu();
}

impl<H: AxVCpuHal> AxArchPerCpu for Aarch64PerCpu<H> {
    fn new(cpu_id: usize) -> AxResult<Self> {
        // Register IRQ handler for this CPU.
        let _ = unsafe { IRQ_HANDLER.current_ref_mut_raw() }
            .set(&|| H::irq_hanlder())
            .map(|_| {});

        Ok(Self {
            cpu_id,
            _phantom: PhantomData,
        })
    }

    fn is_enabled(&self) -> bool {
        HCR_EL2.is_set(HCR_EL2::VM)
    }

    fn hardware_enable(&mut self) -> AxResult {
        // First we save origin `exception_vector_base`.
        // Safety:
        // Todo: take care of `preemption`
        unsafe { ORI_EXCEPTION_VECTOR_BASE.write_current_raw(VBAR_EL2.get() as usize) }

        // Set current `VBAR_EL2` to `exception_vector_base_vcpu`
        // defined in this crate.
        VBAR_EL2.set(exception_vector_base_vcpu as usize as _);

        HCR_EL2.modify(
            HCR_EL2::VM::Enable
                + HCR_EL2::RW::EL1IsAarch64
                + HCR_EL2::IMO::EnableVirtualIRQ
                + HCR_EL2::FMO::EnableVirtualFIQ
                + HCR_EL2::TSC::EnableTrapEl1SmcToEl2,
        );

        // Note that `ICH_HCR_EL2` is not the same as `HCR_EL2`.
        //
        // `ICH_HCR_EL2[0]` controls the virtual CPU interface operation.
        //
        // We leave it for the virtual GIC implementations to decide whether to enable it or not.
        //
        // unsafe {
        //     core::arch::asm! {
        //         "msr ich_hcr_el2, {value:x}",
        //         value = in(reg) 0,
        //     }
        // }

        Ok(())
    }

    fn hardware_disable(&mut self) -> AxResult {
        // Reset `VBAR_EL2` into previous value.
        // Safety:
        // Todo: take care of `preemption`
        VBAR_EL2.set(unsafe { ORI_EXCEPTION_VECTOR_BASE.read_current_raw() } as _);

        HCR_EL2.set(HCR_EL2::VM::Disable.into());
        Ok(())
    }
}
