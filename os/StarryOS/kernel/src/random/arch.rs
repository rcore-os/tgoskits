//! CPU random instructions: Linux `arch_get_random_seed_longs()` falling back
//! to `arch_get_random_longs()` (`arch/*/include/asm/archrandom.h`, v7.2-rc3).
//!
//! An instruction is used only when every online CPU implements it, as with the
//! system-wide `ARM64_HAS_RNG` capability on arm64 and the feature set x86
//! keeps common to all CPUs, so a caller moved between cores never executes it
//! on one that lacks it.

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(test, not(axtest))
))]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
use ax_runtime::hal::{
    cpu_num,
    irq::{CpuId, run_on_cpu_sync},
};

/// Settles which random instructions every online CPU implements. Runs once at
/// boot after the secondary CPUs are up; until then none is used.
pub(super) fn init() {
    imp::init();
}

/// Returns one word from the CPU random instruction, or `None` when not every
/// CPU has one or it reported failure.
pub(super) fn random_long() -> Option<u64> {
    imp::random_long()
}

/// A CPU feature that counts as present only if every online CPU reports it.
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(test, not(axtest))
))]
struct SystemFeature(AtomicBool);

#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    all(test, not(axtest))
))]
impl SystemFeature {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn settle(&self, per_cpu: impl IntoIterator<Item = bool>) {
        let mut cpus = per_cpu.into_iter().peekable();
        let present = cpus.peek().is_some() && cpus.all(|present| present);
        // Absent is the safe answer until this store is seen, so nothing else
        // needs ordering against it.
        self.0.store(present, Ordering::Relaxed);
    }

    fn is_present(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Asks each online CPU whether it implements a feature. A CPU the call cannot
/// reach counts as lacking it.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn each_cpu_has(detect: fn() -> bool) -> impl Iterator<Item = bool> {
    (0..cpu_num()).map(move |cpu| {
        let mut probe = Probe {
            detect,
            present: false,
        };
        // SAFETY: `probe` outlives the synchronous call and nothing else
        // touches it until the call returns.
        let reached = unsafe { run_on_cpu_sync(CpuId(cpu), run_probe, (&raw mut probe).cast()) };
        reached.is_ok() && probe.present
    })
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
struct Probe {
    detect: fn() -> bool,
    present: bool,
}

/// Runs a feature probe on the CPU the cross-CPU call targets. The probes only
/// read identification registers, which that hard-IRQ context allows.
///
/// # Safety
///
/// `arg` must point to a live `Probe` that nothing else accesses until the call
/// returns.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
unsafe fn run_probe(arg: *mut ()) {
    // SAFETY: guaranteed by the caller.
    let probe = unsafe { &mut *arg.cast::<Probe>() };
    probe.present = (probe.detect)();
}

#[cfg(target_arch = "x86_64")]
mod imp {
    use x86::{
        cpuid::CpuId,
        random::{rdrand64, rdseed64},
    };

    use super::{SystemFeature, each_cpu_has};

    /// `RDRAND_RETRY_LOOPS`: RDRAND may fail transiently under contention.
    const RDRAND_RETRY_LOOPS: usize = 10;

    static RDSEED: SystemFeature = SystemFeature::new();
    static RDRAND: SystemFeature = SystemFeature::new();

    pub(super) fn init() {
        RDSEED.settle(each_cpu_has(has_rdseed));
        RDRAND.settle(each_cpu_has(has_rdrand));
    }

    pub(super) fn random_long() -> Option<u64> {
        rdseed().or_else(rdrand)
    }

    fn has_rdseed() -> bool {
        CpuId::new()
            .get_extended_feature_info()
            .is_some_and(|info| info.has_rdseed())
    }

    fn has_rdrand() -> bool {
        CpuId::new()
            .get_feature_info()
            .is_some_and(|info| info.has_rdrand())
    }

    fn rdseed() -> Option<u64> {
        if !RDSEED.is_present() {
            return None;
        }
        let mut value = 0;
        // SAFETY: every online CPU implements RDSEED.
        unsafe { rdseed64(&mut value) }.then_some(value)
    }

    fn rdrand() -> Option<u64> {
        if !RDRAND.is_present() {
            return None;
        }
        (0..RDRAND_RETRY_LOOPS).find_map(|_| {
            let mut value = 0;
            // SAFETY: every online CPU implements RDRAND.
            unsafe { rdrand64(&mut value) }.then_some(value)
        })
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use super::{SystemFeature, each_cpu_has};

    /// `ID_AA64ISAR0_EL1.RNDR` occupies bits [63:60].
    const ID_AA64ISAR0_RNDR_SHIFT: u32 = 60;

    static RNDR: SystemFeature = SystemFeature::new();

    pub(super) fn init() {
        RNDR.settle(each_cpu_has(has_rndr));
    }

    pub(super) fn random_long() -> Option<u64> {
        if !RNDR.is_present() {
            return None;
        }
        let value: u64;
        let ok: u64;
        // SAFETY: every online CPU implements RNDR. It clears NZCV on success
        // and sets Z when no entropy is available.
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
    pub(super) fn init() {}

    pub(super) fn random_long() -> Option<u64> {
        None
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::{SystemFeature, random_long};

    #[test]
    fn a_feature_missing_on_any_cpu_is_absent() {
        let feature = SystemFeature::new();
        assert!(!feature.is_present());
        feature.settle([true, false, true]);
        assert!(!feature.is_present());
        feature.settle([true, true, true]);
        assert!(feature.is_present());
        feature.settle([]);
        assert!(!feature.is_present());
    }

    #[test]
    fn random_long_is_safe_to_query() {
        if let Some(first) = random_long() {
            assert!((0..8).any(|_| random_long().is_some_and(|next| next != first)));
        }
    }
}
