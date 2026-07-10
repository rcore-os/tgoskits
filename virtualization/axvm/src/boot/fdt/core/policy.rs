//! Static target policies consumed by common guest FDT operations.

use alloc::vec::Vec;

use ax_errno::AxResult;
use axvmconfig::AxVMCrateConfig;

pub type RuntimeFdtPatch = fn(&[u8], &crate::AxVMRef, &AxVMCrateConfig) -> AxResult<Vec<u8>>;
pub type ProvidedFdtPatch = fn(&[u8], Option<&[u8]>, &AxVMCrateConfig) -> AxResult<Vec<u8>>;

/// Architecture operations required by common guest FDT processing.
#[derive(Clone, Copy)]
pub struct GuestFdtPolicy {
    pub patch_runtime: RuntimeFdtPatch,
    pub patch_provided: ProvidedFdtPatch,
    pub decode_interrupt: fn(&[u32]) -> Option<u32>,
}
