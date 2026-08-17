use kernutil::StaticCell;

#[derive(Clone, Copy)]
struct BootEntropy {
    seed: [u8; 32],
    source: BootEntropySource,
}

#[derive(Clone, Copy)]
enum BootEntropySource {
    UefiRng,
    FdtRngSeed,
}

static BOOT_ENTROPY: StaticCell<Option<BootEntropy>> = StaticCell::uninit();

pub(crate) fn capture() {
    let entropy = select_boot_entropy(uefi_entropy(), crate::fdt::boot_entropy())
        .map(|(seed, source)| BootEntropy { seed, source });

    match entropy {
        Some(BootEntropy {
            source: BootEntropySource::UefiRng,
            ..
        }) => println!("Boot entropy source: UEFI RNG"),
        Some(BootEntropy {
            source: BootEntropySource::FdtRngSeed,
            ..
        }) => println!("Boot entropy source: FDT /chosen/rng-seed"),
        None => warn!("No trusted boot entropy source is available"),
    }

    // SAFETY: primary_init_early runs once on the boot CPU before secondary
    // CPUs start, and this publication occurs before the MMU is enabled.
    unsafe { BOOT_ENTROPY.init_single_core(entropy) };
}

/// Returns the firmware-provided seed captured for this boot.
///
/// The seed is available only when firmware supplied either the UEFI RNG
/// protocol or an exact 32-byte FDT `/chosen/rng-seed` property. Callers must
/// not substitute replayable timing or address state when this returns `None`.
pub fn boot_entropy() -> Option<[u8; 32]> {
    BOOT_ENTROPY.as_ref().map(|entropy| entropy.seed)
}

fn select_boot_entropy(
    uefi: Option<[u8; 32]>,
    fdt: Option<[u8; 32]>,
) -> Option<([u8; 32], BootEntropySource)> {
    uefi.map(|seed| (seed, BootEntropySource::UefiRng))
        .or_else(|| fdt.map(|seed| (seed, BootEntropySource::FdtRngSeed)))
}

#[cfg(efi)]
fn uefi_entropy() -> Option<[u8; 32]> {
    crate::efi_stub::boot_entropy()
}

#[cfg(not(efi))]
fn uefi_entropy() -> Option<[u8; 32]> {
    None
}

#[cfg(test)]
mod tests {
    use super::{BootEntropySource, select_boot_entropy};

    #[test]
    fn uefi_rng_takes_precedence_over_fdt_rng_seed() {
        let uefi = [0x11; 32];
        let fdt = [0x22; 32];
        let (seed, source) =
            select_boot_entropy(Some(uefi), Some(fdt)).expect("select boot entropy");

        assert_eq!(seed, uefi);
        assert!(matches!(source, BootEntropySource::UefiRng));
    }

    #[test]
    fn fdt_rng_seed_is_used_without_uefi_rng() {
        let fdt = [0x22; 32];
        let (seed, source) = select_boot_entropy(None, Some(fdt)).expect("select boot entropy");

        assert_eq!(seed, fdt);
        assert!(matches!(source, BootEntropySource::FdtRngSeed));
    }

    #[test]
    fn missing_trusted_source_remains_explicit() {
        assert!(select_boot_entropy(None, None).is_none());
    }
}
