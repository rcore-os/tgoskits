use crate::{arch::AxArchVCpuImpl, AxVMHal};
use axvcpu::AxArchVCpu;

/// A part of `AxVCpuConfig`, which represents an architecture-dependent `VCpu`.
///
/// The concrete type of configuration is defined in `AxArchVCpuImpl`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AxArchVCpuConfig<H: AxVMHal> {
    pub create_config: <AxArchVCpuImpl<H> as AxArchVCpu>::CreateConfig,
    pub setup_config: <AxArchVCpuImpl<H> as AxArchVCpu>::SetupConfig,
}

/// A part of `AxVMConfig`, which represents a `VCpu`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AxVCpuConfig<H: AxVMHal> {
    pub arch_config: AxArchVCpuConfig<H>,
    pub bsp_entry: usize,
    pub ap_entry: usize,
}

/// A part of `AxVMCrateConfig`, which represents a `VM`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AxVMConfig<H: AxVMHal> {
    pub cpu_count: usize,
    pub cpu_config: AxVCpuConfig<H>,
    // To be added: memory mapping configuration
    // To be added: device configuration
}

/// The configuration of axvm crate. It's not used yet, may be used in the future.
pub struct AxVMCrateConfig {}
