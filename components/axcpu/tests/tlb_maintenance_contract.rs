//! Guards the aarch64 `flush_tlb` instruction sequences against silent
//! regression.
//!
//! These two sequences are SMP correctness requirements (ARM ARM DDI0487
//! break-before-make + broadcast invalidation) that a normal single-core build
//! or run cannot exercise — the hazard only appears when another core caches a
//! stale translation. So pin the exact instruction strings at the source level,
//! the same way the sibling `*_contract.rs` tests pin ABI/layout invariants.

/// The aarch64 asm module source. `include_str!` reads it on any host target
/// (the module itself is `#[cfg(target_arch = "aarch64")]`, but its text is
/// always present), so this guard runs under `cargo test` on the CI host.
const AARCH64_ASM_SRC: &str = include_str!("../src/aarch64/asm.rs");

#[test]
fn by_va_tlbi_orders_the_pte_store_before_the_invalidate() {
    // `dsb ishst` must precede the by-VA TLBI so the caller's invalidating PTE
    // store is observable to all cores before the TLBI completes; otherwise a
    // walker on another core re-caches the stale entry (break-before-make).
    assert!(
        AARCH64_ASM_SRC.contains("dsb ishst; tlbi vaae1is"),
        "the EL1 by-VA TLBI must be preceded by `dsb ishst`"
    );
    assert!(
        AARCH64_ASM_SRC.contains("dsb ishst; tlbi vae2is"),
        "the EL2 by-VA TLBI must be preceded by `dsb ishst`"
    );
}

#[test]
fn full_el1_flush_is_inner_shareable_broadcast_not_cpu_local() {
    // The full-flush fallback must broadcast to the inner-shareable domain
    // (`vmalle1is`); the CPU-local `vmalle1` leaves sibling cores translating
    // through freed/replaced mappings.
    assert!(
        AARCH64_ASM_SRC.contains("tlbi vmalle1is"),
        "the full EL1 flush must use the inner-shareable broadcast `vmalle1is`"
    );
    // Reject any regression back to the CPU-local form, in any spacing. Note
    // `vmalle1is` never matches these (the char after `vmalle1` is `i`).
    for local in ["vmalle1;", "vmalle1 ", "vmalle1,"] {
        assert!(
            !AARCH64_ASM_SRC.contains(local),
            "the full EL1 flush must not regress to the CPU-local `vmalle1`"
        );
    }
}
