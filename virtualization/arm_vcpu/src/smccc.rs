//! VM-local Arm SMC Calling Convention architecture-call dispatch.

const VERSION_FUNCTION: u32 = 0x8000_0000;
const ARCH_FEATURES_FUNCTION: u32 = 0x8000_0001;
const ARCHITECTURE_CALL_32_RANGE: core::ops::RangeInclusive<u32> = 0x8000_0000..=0x8000_ffff;
const ARCHITECTURE_CALL_64_RANGE: core::ops::RangeInclusive<u32> = 0xc000_0000..=0xc000_ffff;
const VERSION_1_1: u64 = 0x0001_0001;

pub(crate) const NOT_SUPPORTED: u64 = u64::MAX;

/// Handles the architecture-owned SMCCC ranges without entering host firmware.
///
/// SMCCC function identifiers occupy `W0`, so only the low 32 bits of the
/// guest register participate in dispatch. The architecture ranges remain
/// reserved even when a particular call is not implemented; this prevents an
/// unimplemented discovery or mitigation call from becoming a VMM hypercall.
pub(crate) fn architecture_call(function: u64, _feature: u64) -> Option<u64> {
    let function = function as u32;
    match function {
        VERSION_FUNCTION => Some(VERSION_1_1),
        ARCH_FEATURES_FUNCTION => Some(NOT_SUPPORTED),
        function
            if ARCHITECTURE_CALL_32_RANGE.contains(&function)
                || ARCHITECTURE_CALL_64_RANGE.contains(&function) =>
        {
            Some(NOT_SUPPORTED)
        }
        _ => None,
    }
}
