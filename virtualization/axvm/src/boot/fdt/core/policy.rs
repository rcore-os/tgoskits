//! Static target policies consumed by common guest FDT operations.

use alloc::vec::Vec;

use axvmconfig::AxVMCrateConfig;

use crate::AxVmResult;

pub type RuntimeFdtPatch = fn(&[u8], &crate::AxVMRef, &AxVMCrateConfig) -> AxVmResult<Vec<u8>>;

/// Architecture operations required by common guest FDT processing.
#[derive(Clone, Copy)]
pub struct GuestFdtPolicy {
    pub patch_runtime: RuntimeFdtPatch,
}
