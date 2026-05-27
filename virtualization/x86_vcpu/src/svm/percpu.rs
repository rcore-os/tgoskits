use ax_errno::{AxResult, ax_err};
use axvcpu::AxArchPerCpu;

use crate::{
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
pub struct SvmPerCpuState {
    hsave_page: axvisor_api::memory::PhysFrame,
}

impl AxArchPerCpu for SvmPerCpuState {
    fn new(_cpu_id: usize) -> AxResult<Self> {
        Ok(Self {
            hsave_page: unsafe { axvisor_api::memory::PhysFrame::uninit() },
        })
    }

    fn is_enabled(&self) -> bool {
        Msr::IA32_EFER.read() & EFER_SVME != 0
    }

    fn hardware_enable(&mut self) -> AxResult {
        if !has_hardware_support() {
            return ax_err!(Unsupported, "CPU does not support AMD SVM");
        }
        if VmCr::read().contains(VmCrFlags::SVMDIS) {
            return ax_err!(Unsupported, "AMD SVM is disabled by VM_CR");
        }
        if self.is_enabled() {
            return ax_err!(ResourceBusy, "SVM is already turned on");
        }

        enable_xsave();

        self.hsave_page = axvisor_api::memory::PhysFrame::alloc_zero()?;
        let hsave_pa = self.hsave_page.start_paddr().as_usize() as u64;
        unsafe {
            Msr::VM_HSAVE_PA.write(hsave_pa);
            Msr::IA32_EFER.write(Msr::IA32_EFER.read() | EFER_SVME);
        }

        info!("[AxVM] succeeded to turn on SVM (HSAVE @ {:#x}).", hsave_pa);
        Ok(())
    }

    fn hardware_disable(&mut self) -> AxResult {
        if !self.is_enabled() {
            return ax_err!(BadState, "SVM is not enabled");
        }

        unsafe {
            Msr::IA32_EFER.write(Msr::IA32_EFER.read() & !EFER_SVME);
            Msr::VM_HSAVE_PA.write(0);
        }
        self.hsave_page = unsafe { axvisor_api::memory::PhysFrame::uninit() };

        info!("[AxVM] succeeded to turn off SVM.");
        Ok(())
    }
}
