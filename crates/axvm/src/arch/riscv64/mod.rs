pub mod csrs;
mod detect;
mod device_list;
mod devices;
mod regs;
pub mod sbi;
mod vcpu;
mod vm_pages;
mod vmexit;

use self::csrs::{traps, RiscvCsrTrait, CSR};
pub(crate) use self::detect::detect_h_extension as has_hardware_support;
pub use self::device_list::DeviceList as AxArchDeviceList;
pub use self::vcpu::VCpu as AxArchVCpuImpl;
// pub use self::vcpu::VCpuConfig as AxArchVCpuConfig;
pub use self::PerCpu as AxVMArchPerCpuImpl;
use crate::percpu::AxVMArchPerCpu;
use axerrno::AxError;
use axerrno::AxResult;

use crate::AxVMHal;

pub struct PerCpu<H: AxVMHal> {
    _marker: core::marker::PhantomData<H>,
}

impl<H: AxVMHal> AxVMArchPerCpu for PerCpu<H> {
    fn new(_cpu_id: usize) -> AxResult<Self> {
        unsafe {
            setup_csrs();
        }

        Ok(Self {
            _marker: core::marker::PhantomData,
        })
    }

    fn is_enabled(&self) -> bool {
        unimplemented!()
    }

    fn hardware_enable(&mut self) -> AxResult<()> {
        if has_hardware_support() {
            Ok(())
        } else {
            Err(AxError::Unsupported)
        }
    }

    fn hardware_disable(&mut self) -> AxResult<()> {
        unimplemented!()
    }
}

/// Initialize (H)S-level CSRs to a reasonable state.
unsafe fn setup_csrs() {
    // Delegate some synchronous exceptions.
    CSR.hedeleg.write_value(
        traps::exception::INST_ADDR_MISALIGN
            | traps::exception::BREAKPOINT
            | traps::exception::ENV_CALL_FROM_U_OR_VU
            | traps::exception::INST_PAGE_FAULT
            | traps::exception::LOAD_PAGE_FAULT
            | traps::exception::STORE_PAGE_FAULT
            | traps::exception::ILLEGAL_INST,
    );

    // Delegate all interupts.
    CSR.hideleg.write_value(
        traps::interrupt::VIRTUAL_SUPERVISOR_TIMER
            | traps::interrupt::VIRTUAL_SUPERVISOR_EXTERNAL
            | traps::interrupt::VIRTUAL_SUPERVISOR_SOFT,
    );

    // Clear all interrupts.
    CSR.hvip.read_and_clear_bits(
        traps::interrupt::VIRTUAL_SUPERVISOR_TIMER
            | traps::interrupt::VIRTUAL_SUPERVISOR_EXTERNAL
            | traps::interrupt::VIRTUAL_SUPERVISOR_SOFT,
    );

    // clear all interrupts.
    CSR.hcounteren.write_value(0xffff_ffff);

    // enable interrupt
    CSR.sie.write_value(
        traps::interrupt::SUPERVISOR_EXTERNAL
            | traps::interrupt::SUPERVISOR_SOFT
            | traps::interrupt::SUPERVISOR_TIMER,
    );
    debug!("sie: {:#x}", CSR.sie.get_value());
}
