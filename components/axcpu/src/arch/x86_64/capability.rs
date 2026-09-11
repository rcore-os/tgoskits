//! Current CPU identification and raw CPUID leaves.

pub use core::arch::x86_64::CpuidResult;

pub use x86::cpuid::{CpuId, Hypervisor};

/// Reads one CPUID leaf and subleaf from the current CPU.
/// The caller interprets only leaves advertised by the corresponding maximum
/// basic or extended leaf and keeps CPU affinity when combining observations.
pub fn cpuid(leaf: u32, subleaf: u32) -> CpuidResult {
    core::arch::x86_64::__cpuid_count(leaf, subleaf)
}

/// Returns the implemented physical-address width, or None without leaf 80000008.
pub fn physical_address_bits() -> Option<usize> {
    (cpuid(0x8000_0000, 0).eax >= 0x8000_0008).then(|| (cpuid(0x8000_0008, 0).eax & 0xff) as usize)
}
