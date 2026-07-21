//! LoongArch CPU-anchor ownership contract.

const REGISTER: &str = include_str!("../src/register.rs");

#[test]
fn current_anchor_verifies_the_live_r21_against_ks3() {
    let loongarch = REGISTER
        .split_once("target_arch = \"loongarch64\"")
        .expect("LoongArch register backend must exist")
        .1
        .split_once("target_arch = \"arm\"")
        .expect("LoongArch register backend must end before ARM")
        .0;

    assert!(
        loongarch.contains("csrrd {shadow}, 0x33"),
        "reading the CPU anchor must also read the KS3 area-base mirror"
    );
    let semantic = loongarch.split_whitespace().collect::<String>();
    assert!(
        semantic.contains("assert_eq!(area_base,shadow,"),
        "a mismatched live r21 and KS3 must be a fatal CPU-local invariant"
    );
    assert!(
        !loongarch.contains("cpu_area_header_link_address") && !loongarch.contains(".relocate("),
        "reading r21 must return the direct runtime area base"
    );
}
