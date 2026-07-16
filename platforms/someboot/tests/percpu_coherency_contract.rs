//! Source contracts for coherent CPU-local storage and its boot-time publication.

const AARCH64_PTE: &str = include_str!("../src/arch/aarch64/paging/pte.rs");
const SMP: &str = include_str!("../src/smp/mod.rs");

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let source = &source[start..];
    let open = source.find('{').expect("function body must start");
    let mut depth = 0;
    for (offset, character) in source[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function `{signature}`")
}

#[test]
fn aarch64_percpu_alias_is_normal_shareable_memory() {
    let encode = function_body(AARCH64_PTE, "fn from_config(");
    let normal = encode
        .split("MemAttributes::Normal | MemAttributes::PerCpu =>")
        .nth(1)
        .expect("AArch64 must encode normal and CPU-local RAM together")
        .split("MemAttributes::Uncached")
        .next()
        .unwrap();

    assert!(
        normal.contains("PTE::SHAREABLE::INNER"),
        "CPU-local RAM must use Linux-compatible inner-shareable normal-RAM attributes"
    );
    assert!(
        !normal.contains("PTE::SHAREABLE::NON"),
        "a non-shareable CPU-local alias conflicts with the normal direct-map alias"
    );
}

#[test]
fn aarch64_shareability_names_match_the_stage_one_descriptor_encoding() {
    assert!(AARCH64_PTE.contains("RESERVED = 0b01"));
    assert!(AARCH64_PTE.contains("OUTER = 0b10"));
    assert!(AARCH64_PTE.contains("INNER = 0b11"));
}

#[test]
fn late_boot_metadata_publication_never_invalidates_live_cpu_local_values() {
    let publish = function_body(SMP, "pub(crate) fn finalize_secondary_boot_metadata(");
    assert!(publish.contains("DCacheOp::Clean"));
    assert!(publish.contains("size_of::<PerCpuMeta>()"));
    assert!(
        !publish.contains("DCacheOp::CleanInvalidate")
            && !publish.contains("PERCPU_START")
            && !publish.contains("PERCPU_END"),
        "post-allocator boot metadata publication must not touch the live CPU-data or stack \
         regions"
    );
}
