use crate::arch::AxArchVCpuConfig;

/// A part of `AxVMConfig`, which represents a `VCpu`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AxVCpuConfig {
    pub arch_config: AxArchVCpuConfig,
    pub bsp_entry: usize,
    pub ap_entry: usize,
}

/// A part of `AxVMCrateConfig`, which represents a `VM`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AxVMConfig {
    pub cpu_count: usize,
    pub cpu_config: AxVCpuConfig,
}

/// The configuration of axvm crate.
pub struct AxVMCrateConfig {

}
