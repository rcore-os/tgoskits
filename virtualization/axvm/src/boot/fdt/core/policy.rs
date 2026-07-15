//! Static target policies consumed by common guest FDT operations.

use alloc::vec::Vec;

use axvmconfig::AxVMCrateConfig;

use crate::AxVmResult;

pub type RuntimeFdtPatch = fn(&[u8], &crate::AxVMRef, &AxVMCrateConfig) -> AxVmResult<Vec<u8>>;
pub type ProvidedFdtPatch = fn(&[u8], Option<&[u8]>, &AxVMCrateConfig) -> AxVmResult<Vec<u8>>;
pub type PrepareHostIrqRoutes = fn(&mut crate::config::AxVMConfig, Option<&[u8]>) -> AxVmResult;
pub type EnrichGuestInterrupts = fn(&mut crate::config::AxVMConfig, &[u8]) -> AxVmResult;

/// Architecture operations required by common guest FDT processing.
#[derive(Clone, Copy)]
pub struct GuestFdtPolicy {
    pub patch_runtime: RuntimeFdtPatch,
    pub patch_provided: ProvidedFdtPatch,
    pub decode_interrupt: fn(&[u32]) -> Option<u32>,
    pub prepare_host_irq_routes: PrepareHostIrqRoutes,
    pub enrich_guest_interrupts: EnrichGuestInterrupts,
}
