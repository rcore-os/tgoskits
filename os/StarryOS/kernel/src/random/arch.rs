//! CPU random instructions: Linux `arch_get_random_seed_longs()` falling back
//! to `arch_get_random_longs()` (`arch/*/include/asm/archrandom.h`, v7.2-rc3).
//!
//! Each backend probes the CPU feature once and never executes the instruction
//! on a core that lacks it.

/// Returns one word from the CPU random instruction, or `None` when the core
/// has none or it reported failure.
pub(super) fn random_long() -> Option<u64> {
    imp::random_long()
}

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(test, not(axtest))
))]
use core::sync::atomic::{AtomicU8, Ordering};

/// Cached CPU feature probe. Racing first callers observe the same hardware
/// fact, so `Relaxed` ordering is sufficient.
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(test, not(axtest))
))]
struct FeatureProbe(AtomicU8);

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(test, not(axtest))
))]
impl FeatureProbe {
    const UNKNOWN: u8 = 0;
    const ABSENT: u8 = 1;
    const PRESENT: u8 = 2;

    const fn new() -> Self {
        Self(AtomicU8::new(Self::UNKNOWN))
    }

    fn is_present(&self, detect: impl FnOnce() -> bool) -> bool {
        match self.0.load(Ordering::Relaxed) {
            Self::PRESENT => true,
            Self::ABSENT => false,
            _ => {
                let present = detect();
                let state = if present { Self::PRESENT } else { Self::ABSENT };
                self.0.store(state, Ordering::Relaxed);
                present
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod imp {
    use x86::{
        cpuid::CpuId,
        random::{rdrand64, rdseed64},
    };

    use super::FeatureProbe;

    /// `RDRAND_RETRY_LOOPS`: RDRAND may fail transiently under contention.
    const RDRAND_RETRY_LOOPS: usize = 10;

    static RDSEED: FeatureProbe = FeatureProbe::new();
    static RDRAND: FeatureProbe = FeatureProbe::new();

    pub(super) fn random_long() -> Option<u64> {
        rdseed().or_else(rdrand)
    }

    fn rdseed() -> Option<u64> {
        let present = RDSEED.is_present(|| {
            CpuId::new()
                .get_extended_feature_info()
                .is_some_and(|info| info.has_rdseed())
        });
        if !present {
            return None;
        }
        let mut value = 0;
        // SAFETY: the CPUID probe shows RDSEED is implemented on this CPU.
        unsafe { rdseed64(&mut value) }.then_some(value)
    }

    fn rdrand() -> Option<u64> {
        let present = RDRAND.is_present(|| {
            CpuId::new()
                .get_feature_info()
                .is_some_and(|info| info.has_rdrand())
        });
        if !present {
            return None;
        }
        (0..RDRAND_RETRY_LOOPS).find_map(|_| {
            let mut value = 0;
            // SAFETY: the CPUID probe shows RDRAND is implemented on this CPU.
            unsafe { rdrand64(&mut value) }.then_some(value)
        })
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use super::FeatureProbe;

    /// `ID_AA64ISAR0_EL1.RNDR` occupies bits [63:60].
    const ID_AA64ISAR0_RNDR_SHIFT: u32 = 60;

    static RNDR: FeatureProbe = FeatureProbe::new();

    pub(super) fn random_long() -> Option<u64> {
        if !RNDR.is_present(has_rndr) {
            return None;
        }
        let value: u64;
        let ok: u64;
        // SAFETY: the ID_AA64ISAR0_EL1 probe shows RNDR is implemented. RNDR
        // clears NZCV on success and sets Z when no entropy is available.
        unsafe {
            core::arch::asm!(
                "mrs {value}, S3_3_C2_C4_0",
                "cset {ok}, ne",
                value = out(reg) value,
                ok = out(reg) ok,
                options(nostack),
            );
        }
        (ok != 0).then_some(value)
    }

    fn has_rndr() -> bool {
        let isar0: u64;
        // SAFETY: ID_AA64ISAR0_EL1 is a read-only identification register.
        unsafe {
            core::arch::asm!(
                "mrs {isar0}, ID_AA64ISAR0_EL1",
                isar0 = out(reg) isar0,
                options(nomem, nostack, preserves_flags),
            );
        }
        (isar0 >> ID_AA64ISAR0_RNDR_SHIFT) & 0xf != 0
    }
}

// Linux reads the RISC-V Zkr `seed` CSR only after firmware ISA strings report
// the extension, and LoongArch has no random instruction. Starry has no such
// probe, so these cores credit nothing from the CPU.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod imp {
    pub(super) fn random_long() -> Option<u64> {
        None
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::{FeatureProbe, random_long};

    #[test]
    fn probe_detects_once() {
        for present in [true, false] {
            let probe = FeatureProbe::new();
            let mut calls = 0;
            assert_eq!(
                probe.is_present(|| {
                    calls += 1;
                    present
                }),
                present
            );
            assert_eq!(
                probe.is_present(|| {
                    calls += 1;
                    !present
                }),
                present
            );
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn random_long_is_safe_to_query() {
        if let Some(first) = random_long() {
            assert!((0..8).any(|_| random_long().is_some_and(|next| next != first)));
        }
    }
}
