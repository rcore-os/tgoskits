//! Static target policies consumed by common guest FDT operations.

use std::vec::Vec;

use axdevice_base::InterruptTriggerMode;
use axvmconfig::GuestConfig;

use crate::AxVmResult;

pub type RuntimeFdtPatch = fn(&[u8], &crate::AxVMRef, &GuestConfig) -> AxVmResult<Vec<u8>>;
pub type ProvidedFdtPatch = fn(&[u8], Option<&[u8]>, &GuestConfig) -> AxVmResult<Vec<u8>>;

/// Interrupt source and trigger semantics decoded from one firmware specifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedInterrupt {
    /// Architecture-local interrupt source number.
    pub source: u32,
    /// Trigger mode declared by firmware.
    pub trigger: InterruptTriggerMode,
}

/// Architecture operations required by common guest FDT processing.
#[derive(Clone, Copy)]
pub struct GuestFdtPolicy {
    pub patch_runtime: RuntimeFdtPatch,
    pub patch_provided: ProvidedFdtPatch,
    pub decode_interrupt: fn(&[u32]) -> Option<DecodedInterrupt>,
    pub resolve_cpu_index: fn(usize) -> Option<usize>,
    pub host_cpu_count: fn() -> usize,
}
