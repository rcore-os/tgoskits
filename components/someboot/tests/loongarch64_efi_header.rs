const LOONGARCH64_HEAD: &str = include_str!("../src/arch/loongarch64/head.rs");

#[test]
fn loongarch64_efi_image_base_stays_position_independent() {
    assert!(
        LOONGARCH64_HEAD.contains(".quad {phys_link_kaddr}\",      // PHYS_LINK_KADDR"),
        "LoongArch direct boot metadata should still record PHYS_LINK_KADDR"
    );
    assert!(
        LOONGARCH64_HEAD.contains(".quad {pe_image_base}\",        // ImageBase"),
        "LoongArch UEFI PE ImageBase should be configurable for board-specific firmware quirks"
    );
    assert!(
        !LOONGARCH64_HEAD.contains(".quad {phys_link_kaddr}\",      // ImageBase"),
        "LoongArch UEFI PE ImageBase must not reuse the direct-boot physical link address"
    );
}
