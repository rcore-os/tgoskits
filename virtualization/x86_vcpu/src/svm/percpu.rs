use core::marker::PhantomData;

use crate::{
    X86HostOps, X86VcpuResult,
    host::PhysFrame,
    msr::Msr,
    svm::{
        flags::{VmCr, VmCrFlags},
        has_hardware_support,
    },
    xstate::enable_xsave,
};

const EFER_SVME: u64 = 1 << 12;

/// Per-CPU AMD SVM state.
#[derive(Debug)]
pub struct SvmPerCpuState<H: X86HostOps> {
    hsave_page: PhysFrame<H>,
    _host: PhantomData<fn() -> H>,
}

impl<H: X86HostOps> SvmPerCpuState<H> {
    pub fn new(_cpu_id: usize) -> X86VcpuResult<Self> {
        Ok(Self {
            hsave_page: unsafe { PhysFrame::<H>::uninit() },
            _host: PhantomData,
        })
    }

    pub fn is_enabled(&self) -> bool {
        Msr::IA32_EFER.read() & EFER_SVME != 0
    }

    pub fn hardware_enable(&mut self) -> X86VcpuResult {
        if !has_hardware_support() {
            return x86_err!(Unsupported, "CPU does not support AMD SVM");
        }
        if VmCr::read().contains(VmCrFlags::SVMDIS) {
            return x86_err!(Unsupported, "AMD SVM is disabled by VM_CR");
        }
        if self.is_enabled() {
            return x86_err!(ResourceBusy, "SVM is already turned on");
        }

        enable_xsave();

        self.hsave_page = PhysFrame::<H>::alloc_zero()?;
        let hsave_pa = self.hsave_page.start_paddr().as_usize() as u64;
        unsafe {
            Msr::VM_HSAVE_PA.write(hsave_pa);
            Msr::IA32_EFER.write(Msr::IA32_EFER.read() | EFER_SVME);
        }

        info!("[AxVM] succeeded to turn on SVM (HSAVE @ {:#x}).", hsave_pa);
        Ok(())
    }

    pub fn hardware_disable(&mut self) -> X86VcpuResult {
        if !self.is_enabled() {
            return x86_err!(BadState, "SVM is not enabled");
        }

        unsafe {
            Msr::IA32_EFER.write(Msr::IA32_EFER.read() & !EFER_SVME);
            Msr::VM_HSAVE_PA.write(0);
        }
        self.hsave_page = unsafe { PhysFrame::<H>::uninit() };

        info!("[AxVM] succeeded to turn off SVM.");
        Ok(())
    }
}
