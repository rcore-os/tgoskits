//! Pure AArch64 stage-2 address-width selection.

/// Returns the usable guest physical-address width for a single-root, 4 KiB
/// stage-2 page table.
///
/// Arm requires the configured IPA size to be no wider than the implemented
/// physical-address size. A table can therefore have more structural capacity
/// than the target CPU permits `VTCR_EL2.T0SZ` to expose.
pub(crate) fn stage2_gpa_bits(levels: usize, pa_bits: usize) -> Option<usize> {
    let table_capacity_bits = match levels {
        3 => 39,
        4 => 48,
        _ => return None,
    };
    Some(table_capacity_bits.min(pa_bits))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_ipa_width_never_exceeds_host_physical_address_width() {
        assert_eq!(stage2_gpa_bits(3, 36), Some(36));
        assert_eq!(stage2_gpa_bits(3, 44), Some(39));
        assert_eq!(stage2_gpa_bits(4, 44), Some(44));
        assert_eq!(stage2_gpa_bits(4, 48), Some(48));
    }

    #[test]
    fn unsupported_stage2_level_count_has_no_address_width() {
        assert_eq!(stage2_gpa_bits(2, 40), None);
        assert_eq!(stage2_gpa_bits(5, 48), None);
    }
}
