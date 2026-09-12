//! Linux perf event selection and CPU-cluster policy.
use ax_cpu::pmu::{EventSupport, PmuInfo};

pub(super) const fn event_supported_by(info: PmuInfo, event: u16) -> bool {
    !matches!(info.event_support(event), EventSupport::Unsupported)
}

/// CPU-cluster class derived from `MIDR_EL1`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClusterId {
    /// Arm Cortex-A55, used by the RK3588 LITTLE cluster.
    CortexA55,
    /// Arm Cortex-A76, used by the RK3588 big clusters.
    CortexA76,
    /// Any other CPU, retaining its implementer and part number.
    Other {
        /// `MIDR_EL1.Implementer`.
        implementer: u8,
        /// `MIDR_EL1.PartNum`.
        part: u16,
    },
}

/// Classifies a raw `MIDR_EL1` using the same implementer/part fields as Linux.
pub const fn classify_midr(midr: u64) -> ClusterId {
    let implementer = ((midr >> 24) & 0xff) as u8;
    let part = ((midr >> 4) & 0xfff) as u16;
    match (implementer, part) {
        (0x41, 0xd05) => ClusterId::CortexA55,
        (0x41, 0xd0b) => ClusterId::CortexA76,
        _ => ClusterId::Other { implementer, part },
    }
}

/// Maps a Linux generic hardware event using one CPU's cached PMCEID bitmap.
pub const fn hw_event_to_arm_with(info: PmuInfo, hw_id: u32) -> Option<u16> {
    match hw_id {
        // PERF_COUNT_HW_CPU_CYCLES => CPU_CYCLES.
        0 if event_supported_by(info, 0x11) => Some(0x11),
        // PERF_COUNT_HW_INSTRUCTIONS => INST_RETIRED.
        1 if event_supported_by(info, 0x08) => Some(0x08),
        // PERF_COUNT_HW_CACHE_REFERENCES => L1D_CACHE.
        2 if event_supported_by(info, 0x04) => Some(0x04),
        // PERF_COUNT_HW_CACHE_MISSES => L1D_CACHE_REFILL.
        3 if event_supported_by(info, 0x03) => Some(0x03),
        // Linux prefers BR_RETIRED and falls back to PC_WRITE_RETIRED.
        4 if event_supported_by(info, 0x21) => Some(0x21),
        4 if event_supported_by(info, 0x0c) => Some(0x0c),
        4 => None,
        // PERF_COUNT_HW_BRANCH_MISSES => BR_MIS_PRED.
        5 if event_supported_by(info, 0x10) => Some(0x10),
        // PERF_COUNT_HW_BUS_CYCLES => BUS_CYCLES.
        6 if event_supported_by(info, 0x1D) => Some(0x1D),
        // PERF_COUNT_HW_STALLED_CYCLES_FRONTEND => STALL_FRONTEND.
        7 if event_supported_by(info, 0x23) => Some(0x23),
        // PERF_COUNT_HW_STALLED_CYCLES_BACKEND => STALL_BACKEND.
        8 if event_supported_by(info, 0x24) => Some(0x24),
        // PERF_COUNT_HW_REF_CPU_CYCLES (9) and anything else are unmapped.
        _ => None,
    }
}

/// Failure class returned by [`hw_cache_to_arm`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheEventError {
    /// A cache, operation, or result selector is outside the Linux UAPI range.
    Invalid,
    /// The selector is valid but the generic ARM PMUv3 map has no event for it.
    Unsupported,
}

/// Maps Linux `PERF_TYPE_HW_CACHE` onto the generic ARM PMUv3 event table.
///
/// Cortex-A55 and Cortex-A76 use Linux's `PMUV3_INIT_SIMPLE` setup, so only
/// generic read operations are accepted. Microarchitecture-specific A53 maps
/// are intentionally not applied to A55/A76.
pub const fn hw_cache_to_arm(config: u64) -> Result<u16, CacheEventError> {
    let cache = (config & 0xff) as u8;
    let operation = ((config >> 8) & 0xff) as u8;
    let result = ((config >> 16) & 0xff) as u8;
    if cache >= 7 || operation >= 3 || result >= 2 {
        return Err(CacheEventError::Invalid);
    }
    if operation != 0 {
        return Err(CacheEventError::Unsupported);
    }
    match (cache, result) {
        (0, 0) => Ok(0x04), // L1D access
        (0, 1) => Ok(0x03), // L1D refill
        (1, 0) => Ok(0x14), // L1I access
        (1, 1) => Ok(0x01), // L1I refill
        (2, 0) => Ok(0x36), // last-level read
        (2, 1) => Ok(0x37), // last-level read miss
        (3, 0) => Ok(0x25), // DTLB access
        (3, 1) => Ok(0x05), // DTLB refill
        (4, 0) => Ok(0x26), // ITLB access
        (4, 1) => Ok(0x02), // ITLB refill
        (5, 0) => Ok(0x12), // branch prediction access
        (5, 1) => Ok(0x10), // branch misprediction
        _ => Err(CacheEventError::Unsupported),
    }
}
