//! Source-level checks for the AArch64 page-table publication and TLBI order.
//!
//! Privileged TLB instructions cannot run in host tests. Keep this contract on
//! every CI host so refactors cannot silently remove the barriers around the
//! architecture boundary.

const ASM: &str = include_str!("../src/aarch64/asm.rs");

fn function_source(name: &str, next_name: &str) -> &'static str {
    let start_marker = format!("pub fn {name}");
    let end_marker = format!("pub fn {next_name}");
    let start = ASM
        .find(&start_marker)
        .unwrap_or_else(|| panic!("missing AArch64 function `{name}`"));
    let rest = &ASM[start..];
    let end = rest
        .find(&end_marker)
        .unwrap_or_else(|| panic!("missing AArch64 function `{next_name}` after `{name}`"));
    &rest[..end]
}

#[test]
fn cross_cpu_shootdown_has_an_explicit_page_table_write_barrier() {
    let source = function_source("synchronize_page_table_writes", "flush_tlb");

    assert!(
        source.contains("dsb ishst"),
        "cross-CPU invalidation must publish PTE writes before any IPI or TLBI"
    );
}

#[test]
fn local_tlbi_orders_page_table_writes_before_reclaim() {
    let source = function_source("flush_tlb", "update_mmu_cache");

    for sequence in [
        "dsb nshst; tlbi vaae1, {}; dsb nsh; isb",
        "dsb nshst; tlbi vae2, {}; dsb nsh; isb",
        "dsb nshst; tlbi vmalle1; dsb nsh; isb",
        "dsb nshst; tlbi alle2; dsb nsh; isb",
    ] {
        assert!(
            source.contains(sequence),
            "AArch64 local invalidation is missing ordered sequence `{sequence}`"
        );
    }

    assert!(
        !source.contains("vaae1is") && !source.contains("vae2is"),
        "the CPU-local API must not bypass the runtime CPU-mask boundary"
    );
}
