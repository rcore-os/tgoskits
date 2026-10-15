//! Contract for the shared CPU refill entry installed by someboot.
//!
//! Boot vectors moved to ax-cpu in #2363. Ordinary boot exceptions are now
//! fatal; the former per-vector execute-access reporter no longer exists.

const REFILL: &str =
    include_str!("../../../components/axcpu/src/arch/loongarch64/entry/tlb_refill.S");
const WALKER: &str = include_str!("../../../components/axcpu/src/arch/loongarch64/entry/walker.S");

#[test]
fn missing_page_table_level_installs_an_invalid_entry_pair() {
    assert_eq!(
        WALKER.matches("beqz $t0, \\invalid").count(),
        3,
        "every directory lookup must stop before dereferencing a zero entry"
    );
    assert!(REFILL.contains("AX_CPU_WALK_4K .Ltlb_invalid"));
    let invalid = REFILL.split_once(".Ltlb_invalid:").unwrap().1;
    let invalid = invalid.split_once(".Ltlb_refill:").unwrap().0;
    assert!(
        invalid.contains("csrwr   $r0, 0x8c") && invalid.contains("csrwr   $r0, 0x8d"),
        "a missing directory must install two zero, invalid TLBRELO values"
    );
}
