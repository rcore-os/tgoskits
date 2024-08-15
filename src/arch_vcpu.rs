use axerrno::AxResult;

use super::{AxVCpuExitReason, GuestPhysAddr, HostPhysAddr};

/// A trait for architecture-specific vcpu.
///
/// This trait is an abstraction for virtual CPUs of different architectures.
pub trait AxArchVCpu: Sized {
    /// The configuration for creating a new [`AxArchVCpu`]. Used by [`AxArchVCpu::new`].
    type CreateConfig;
    /// The configuration for setting up a created [`AxArchVCpu`]. Used by [`AxArchVCpu::setup`].
    type SetupConfig;

    /// Create a new `AxArchVCpu`.
    fn new(config: Self::CreateConfig) -> AxResult<Self>;

    /// Set the entry point of the vcpu.
    ///
    /// It's guaranteed that this function is called only once, before [`AxArchVCpu::setup`] being called.
    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult;
    /// Set the EPT root of the vcpu.
    ///
    /// It's guaranteed that this function is called only once, before [`AxArchVCpu::setup`] being called.
    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult;
    /// Setup the vcpu.
    ///
    /// It's guaranteed that this function is called only once, after [`AxArchVCpu::set_entry`] and [`AxArchVCpu::set_ept_root`] being called.
    fn setup(&mut self, config: Self::SetupConfig) -> AxResult;

    /// Run the vcpu until a vm-exit occurs.
    fn run(&mut self) -> AxResult<AxVCpuExitReason>;
    /// Bind the vcpu to the current physical CPU.
    fn bind(&mut self) -> AxResult;
    /// Unbind the vcpu from the current physical CPU.
    fn unbind(&mut self) -> AxResult;
}
