use axerrno::AxResult;

use super::AxArchVCpuImpl;
use crate::AxVMHal;

pub struct AxArchDeviceList<H: AxVMHal> {
    _marker: core::marker::PhantomData<H>,
}

impl<H: AxVMHal> AxArchDeviceList<H> {
    pub fn new() -> Self {
        Self {
            _marker: core::marker::PhantomData,
        }
    }

    pub fn vmexit_handler(
        &self,
        _arch_vcpu: &mut AxArchVCpuImpl<H>,
        _exit_reason: axvcpu::AxVCpuExitReason,
    ) -> AxResult {
        Ok(())
    }
}
