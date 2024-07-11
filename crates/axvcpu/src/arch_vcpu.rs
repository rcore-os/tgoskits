use axerrno::AxResult;

use super::{AxArchVCpuExitReason, GuestPhysAddr, HostPhysAddr};

pub trait AxArchVCpu: Sized {
    type CreateConfig;
    type SetupConfig;

    /// Create a new `AxArchVCpu`.
    fn new(config: Self::CreateConfig) -> AxResult<Self>;

    /// Set the entry point of the vcpu.
    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult;
    /// Set the EPT root of the vcpu.
    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult;
    /// Setup the vcpu. It's guaranteed that this function is called only once, and after `set_entry` and `set_ept_root`.
    fn setup(&mut self, config: Self::SetupConfig) -> AxResult;

    fn run(&mut self) -> AxResult<AxArchVCpuExitReason>;
    fn bind(&mut self) -> AxResult;
    fn unbind(&mut self) -> AxResult;
}
