//! Per-architecture hardware entropy source for seeding the `/dev/urandom`
//! CSPRNG.
//!
//! [`arch_hw_entropy`] returns one 64-bit word of CPU-provided entropy when the
//! running core exposes a hardware random generator, and `None` otherwise. It is
//! a best-effort strengthening of [`super::random_seed`]: the ChaCha20 stream is
//! always seeded, but folding a hardware word in makes the seed unpredictable
//! rather than derived only from timing/counter/stack state. This matters
//! because `/dev/urandom` backs the ELF `AT_RANDOM` bytes, which seed the libc
//! stack-smashing canary and pointer guard.
//!
//! Detection is cached after first use so the feature probe (CPUID / ID
//! register read) runs once. Every hardware access is guarded by that probe, so
//! the instructions never execute on a core that does not implement them, and
//! an unavailable source degrades to `None` without panicking.
//!
//! Linux reference: `drivers/char/random.c` mixes `arch_get_random_seed_longs`
//! (per-arch `archrandom.h`) into the entropy pool. The retry counts and status
//! decoding below mirror those headers (v7.2.0-rc3).

/// Returns one word of hardware entropy, or `None` when the current
/// architecture and core provide no usable hardware random source.
pub(super) fn arch_hw_entropy() -> Option<u64> {
    arch::hw_entropy()
}

/// `FeatureProbe` is used by the arch backends that gate a privileged
/// instruction (x86_64, aarch64) and by the test helper on every arch.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64", test, axtest))]
use core::sync::atomic::{AtomicU8, Ordering};

/// Cached tri-state feature probe: `UNKNOWN` until first queried, then latched
/// to `PRESENT` or `ABSENT`. Shared by the arch backends that gate a privileged
/// instruction on a one-time CPU-feature check.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64", test, axtest))]
struct FeatureProbe(AtomicU8);

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64", test, axtest))]
impl FeatureProbe {
    const UNKNOWN: u8 = 0;
    const ABSENT: u8 = 1;
    const PRESENT: u8 = 2;

    const fn new() -> Self {
        Self(AtomicU8::new(Self::UNKNOWN))
    }

    /// Returns whether the feature is present, running `detect` at most once.
    /// The cache is a hint, so `Relaxed` ordering is sufficient: a racing pair
    /// of first callers may both run `detect`, but they observe the same
    /// hardware fact and store the same value.
    fn is_present(&self, detect: impl FnOnce() -> bool) -> bool {
        match self.0.load(Ordering::Relaxed) {
            Self::PRESENT => true,
            Self::ABSENT => false,
            _ => {
                let present = detect();
                let cached = if present { Self::PRESENT } else { Self::ABSENT };
                self.0.store(cached, Ordering::Relaxed);
                present
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod arch {
    use x86::{
        cpuid::CpuId,
        random::{rdrand64, rdseed64},
    };

    use super::FeatureProbe;

    /// RDRAND may momentarily fail under heavy contention; Linux retries it up
    /// to this many times (`RDRAND_RETRY_LOOPS`, `arch/x86/include/asm/archrandom.h`).
    const RDRAND_RETRY_LOOPS: u32 = 10;

    static RDRAND: FeatureProbe = FeatureProbe::new();
    static RDSEED: FeatureProbe = FeatureProbe::new();

    /// Prefer RDSEED (a true non-deterministic source, the better fit for
    /// seeding a PRNG) and fall back to RDRAND, matching Linux preferring
    /// `arch_get_random_seed_longs` for pool seeding.
    pub(super) fn hw_entropy() -> Option<u64> {
        rdseed().or_else(rdrand)
    }

    fn rdseed() -> Option<u64> {
        if !RDSEED.is_present(has_rdseed) {
            return None;
        }
        let mut value = 0u64;
        // SAFETY: guarded by the cached CPUID RDSEED probe, so the instruction
        // is implemented on this core. `rdseed64` writes `value` and reports
        // success via the carry flag.
        unsafe { rdseed64(&mut value) }.then_some(value)
    }

    fn rdrand() -> Option<u64> {
        if !RDRAND.is_present(has_rdrand) {
            return None;
        }
        (0..RDRAND_RETRY_LOOPS).find_map(|_| {
            let mut value = 0u64;
            // SAFETY: guarded by the cached CPUID RDRAND probe, so the
            // instruction is implemented on this core. `rdrand64` writes
            // `value` and reports success via the carry flag.
            unsafe { rdrand64(&mut value) }.then_some(value)
        })
    }

    fn has_rdrand() -> bool {
        CpuId::new()
            .get_feature_info()
            .is_some_and(|info| info.has_rdrand())
    }

    fn has_rdseed() -> bool {
        CpuId::new()
            .get_extended_feature_info()
            .is_some_and(|info| info.has_rdseed())
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    use super::FeatureProbe;

    /// `ID_AA64ISAR0_EL1.RNDR` occupies bits [63:60]; a non-zero field means
    /// FEAT_RNG (RNDR/RNDRRS) is implemented (`arch/arm64/include/asm/archrandom.h`,
    /// `__early_cpu_has_rndr`).
    const ID_AA64ISAR0_RNDR_SHIFT: u32 = 60;

    static RNDR: FeatureProbe = FeatureProbe::new();

    pub(super) fn hw_entropy() -> Option<u64> {
        if !RNDR.is_present(has_rndr) {
            return None;
        }
        read_rndr()
    }

    /// Reads the RNDR system register. RNDR clears `PSTATE.NZCV` on success and
    /// sets `Z` (NZCV = 0b0100) when no entropy is available; `cset ne` turns
    /// that into a success flag, mirroring Linux's `__arm64_rndr`.
    fn read_rndr() -> Option<u64> {
        let value: u64;
        let ok: u64;
        // SAFETY: guarded by the cached ID_AA64ISAR0_EL1 probe, so RNDR
        // (S3_3_C2_C4_0) is implemented. The read has no side effects beyond
        // the flags it reports; `cc` marks NZCV clobbered.
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
        // SAFETY: ID_AA64ISAR0_EL1 is a read-only CPU-identification register
        // always accessible at EL1 (the kernel's exception level).
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

// RISC-V exposes hardware entropy through the Zkr `seed` CSR (0x015), but that
// register is only reachable from S-mode when M-mode firmware delegates it via
// `mseccfg.sseed`/`sstateen`, and StarryOS has neither a way to detect the Zkr
// extension from S-mode (`misa` is M-mode only, and there is no DT/SBI
// `riscv,isa` parsing) nor a fault-recoverable probe for kernel-mode illegal
// instructions. Reading `seed` without that guarantee would trap and abort
// boot, so - as a best-effort fallback matching the loongarch64 case - RISC-V
// declines to the timing-based seed rather than risk an illegal-instruction
// trap. Linux instead relies on `riscv_has_extension_likely(RISCV_ISA_EXT_ZKR)`,
// which is populated from firmware-provided ISA strings unavailable here.
//
// loongarch64 has no standard architectural hardware RNG instruction, so it also
// declines to the timing-based seed.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod arch {
    pub(super) fn hw_entropy() -> Option<u64> {
        None
    }
}

#[cfg(any(test, axtest))]
pub(super) fn hw_entropy_probe_is_safe_for_test() -> bool {
    // On every architecture the probe must complete without panicking. Where a
    // hardware source exists it yields `Some`; where it does not (or under a
    // TCG model that omits RDRAND/RNDR) it yields `None`. Both outcomes are
    // valid; the contract under test is that querying it is always safe and,
    // when present, never produces an obviously-broken constant.
    match arch_hw_entropy() {
        None => true,
        Some(first) => {
            // A working source must not be pinned to the trivial all-zero or
            // all-ones word.
            if first == 0 || first == u64::MAX {
                return false;
            }
            // Fresh draws must not be a stuck constant. Allow a few retries so a
            // single unlucky repeat (independent draws can collide) does not
            // fail the test, but a source that only ever returns `first` does.
            (0..8).any(|_| arch_hw_entropy().is_some_and(|next| next != first))
        }
    }
}

#[cfg(any(test, axtest))]
pub(super) fn feature_probe_caches_first_result_for_test() -> bool {
    let probe = FeatureProbe::new();

    // First query runs `detect`; a present feature latches to PRESENT.
    let mut detect_calls = 0;
    let present = probe.is_present(|| {
        detect_calls += 1;
        true
    });

    // Subsequent queries must reuse the cached value without re-detecting.
    let cached = probe.is_present(|| {
        detect_calls += 1;
        false
    });

    present && cached && detect_calls == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_caches_and_avoids_redetect() {
        assert!(feature_probe_caches_first_result_for_test());
    }

    #[test]
    fn absent_feature_latches_without_redetect() {
        let probe = FeatureProbe::new();
        let mut calls = 0;
        let first = probe.is_present(|| {
            calls += 1;
            false
        });
        let second = probe.is_present(|| {
            calls += 1;
            true
        });
        assert!(!first);
        assert!(!second);
        assert_eq!(calls, 1);
    }

    #[test]
    fn entropy_probe_never_panics() {
        // The host runs this as a std test; the arch backend for the host target
        // must return safely regardless of the underlying CPU.
        assert!(hw_entropy_probe_is_safe_for_test());
    }
}
