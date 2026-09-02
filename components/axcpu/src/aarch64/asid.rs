/// Returns the ASID capacity actually configured for the EL1 translation regime.
///
/// `ID_AA64MMFR0_EL1.ASIDBits == 2` advertises 16-bit ASIDs, but those bits are
/// usable only after the boot owner selects them with `TCR_EL1.AS == 1`.
/// Every other combination retains the mandatory 8-bit mode.
pub(super) const fn configured_tag_capacity(asid_bits_encoding: u64, tcr_asid_size: u64) -> u32 {
    if asid_bits_encoding == 2 && tcr_asid_size == 1 {
        1 << 16
    } else {
        1 << 8
    }
}
