//! Pure LoongArch CPUCFG virtual-address geometry regression.

#[path = "../src/arch/loongarch64/virtual_address.rs"]
mod virtual_address;

use virtual_address::LoongArchVirtualAddressLayout;

#[test]
fn cpucfg_valen_limits_both_canonical_halves() {
    let layout = LoongArchVirtualAddressLayout::from_valen(40)
        .expect("LA64 VALEN=40 must be supported by the four-level walker");

    assert_eq!(layout.lower_end(), 1usize << 39);
    assert_eq!(layout.upper_start(), 0usize.wrapping_sub(1usize << 39));
    assert!(LoongArchVirtualAddressLayout::from_valen(12).is_err());
    assert!(LoongArchVirtualAddressLayout::from_valen(49).is_err());
}
