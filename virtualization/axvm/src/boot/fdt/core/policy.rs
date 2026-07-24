//! Static target policies consumed by common guest FDT operations.

use alloc::vec::Vec;

use axvmconfig::AxVMCrateConfig;
use fdt_edit::Fdt;

use super::tree::FdtTree;
use crate::AxVmResult;

pub type RuntimeFdtPatch = fn(&[u8], &crate::AxVMRef, &AxVMCrateConfig) -> AxVmResult<Vec<u8>>;
pub type ProvidedFdtPatch = fn(&[u8], Option<&[u8]>, &AxVMCrateConfig) -> AxVmResult<Vec<u8>>;
pub(crate) type HostDerivedFdtNormalize = fn(&Fdt, &mut FdtTree) -> AxVmResult;

/// Architecture operations required by common guest FDT processing.
#[derive(Clone, Copy)]
pub struct GuestFdtPolicy {
    pub patch_runtime: RuntimeFdtPatch,
    pub patch_provided: ProvidedFdtPatch,
    pub decode_interrupt: fn(&[u32]) -> Option<u32>,
    pub(crate) normalize_host_derived: HostDerivedFdtNormalize,
}
