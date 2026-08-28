//! Source contracts for the LoongArch TLB refill vector retained after boot.

const LOONGARCH_TRAP: &str = include_str!("../src/arch/loongarch64/trap.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .unwrap_or_else(|| panic!("missing section start `{start}`"));
    let tail = &source[start..];
    let end = tail
        .find(end)
        .unwrap_or_else(|| panic!("missing section end `{end}` after `{start}`"));
    &tail[..end]
}

#[test]
fn missing_page_table_level_installs_an_invalid_entry_pair() {
    let refill = section(
        LOONGARCH_TRAP,
        ".Lsomeboot_handle_tlb_refill:",
        ".set exc_num, 81",
    );

    assert_eq!(
        refill
            .matches("beqz    $t0, .Lsomeboot_tlb_invalid")
            .count(),
        3,
        "every directory lookup must stop before dereferencing a zero entry"
    );
    let invalid_entry = section(refill, ".Lsomeboot_tlb_invalid:", ".Lsomeboot_tlb_fill:");
    assert!(
        invalid_entry.contains("csrwr   $r0, 0x8C") && invalid_entry.contains("csrwr   $r0, 0x8D"),
        "a missing directory must install two zero, invalid TLBRELO values"
    );
}

#[test]
fn instruction_page_fault_vectors_preserve_execute_access() {
    let fetch_invalid = section(LOONGARCH_TRAP, "\"handle_exc_3:\"", "\"handle_exc_4:\"");
    let non_executable = section(LOONGARCH_TRAP, "\"handle_exc_6:\"", "\"handle_exc_7:\"");

    assert!(
        fetch_invalid.contains("\"li.d    $a1, 2\""),
        "TLBI must report an execute access"
    );
    assert!(
        non_executable.contains("\"li.d    $a1, 2\""),
        "TLBNX must report an execute access"
    );
    assert!(
        LOONGARCH_TRAP.contains("2 => \"execute\""),
        "the Rust page-fault reporter must decode execute accesses"
    );
}
