//! LoongArch CPU-anchor ownership contract.

const REGISTER: &str = include_str!("../src/register/loongarch64.rs");

#[test]
fn current_anchor_verifies_the_live_r21_against_ks3() {
    assert!(
        REGISTER.contains("csrrd {shadow}, 0x33"),
        "reading the CPU anchor must also read the KS3 area-base mirror"
    );
    let semantic = REGISTER.split_whitespace().collect::<String>();
    assert!(
        semantic.contains("assert_eq!(area_base,shadow,"),
        "a mismatched live r21 and KS3 must be a fatal CPU-local invariant"
    );
    assert!(
        !REGISTER.contains("cpu_area_header_link_address") && !REGISTER.contains(".relocate("),
        "reading r21 must return the direct runtime area base"
    );
}
