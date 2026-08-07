const EXCEPTION_ASSEMBLY: &str = include_str!("exception.S");
const EXCEPTION_RUST: &str = include_str!("exception.rs");
const VCPU: &str = include_str!("vcpu.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start {start:?}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing section end {end:?}"))
        .0
}

fn assert_in_order(source: &str, operations: &[&str]) {
    let mut cursor = 0;
    for operation in operations {
        let offset = source[cursor..]
            .find(operation)
            .unwrap_or_else(|| panic!("missing ordered operation {operation:?}"));
        cursor += offset + operation.len();
    }
}

#[test]
fn guest_exit_preserves_the_host_percpu_register_allocation() {
    for allocation in [
        ".equ HOST_TRAP_KSP_KS, 0x30",
        ".equ HOST_TRAP_T0_KS,  0x31",
        ".equ HOST_TRAP_T1_KS,  0x32",
        ".equ HOST_PERCPU_KS,  0x33",
        ".equ HOST_VCPU_KS,   0x34",
        ".equ HOST_VCPU_TMP_KS, 0x35",
    ] {
        assert!(
            EXCEPTION_ASSEMBLY.contains(allocation),
            "missing register allocation {allocation:?}"
        );
    }

    let save_guest = section(EXCEPTION_ASSEMBLY, ".macro SAVE_GUEST_REGS", ".endm");
    assert_in_order(save_guest, &["st.d    $r21", "RESTORE_HOST_PERCPU"]);
    assert_eq!(
        EXCEPTION_ASSEMBLY
            .matches("\n    SAVE_GUEST_REGS\n")
            .count(),
        4
    );
    assert!(!VCPU.contains("st.d $r21"));
    assert!(!EXCEPTION_RUST.contains("ld.d $r21"));
}

#[test]
fn vm_exit_restores_host_tls_before_returning_to_rust() {
    let trampoline = section(
        EXCEPTION_RUST,
        "unsafe extern \"C\" fn vmexit_trampoline",
        "ctx_size = const core::mem::size_of::<LoongArchContextFrame>()",
    );
    assert_in_order(trampoline, &["ld.d $tp, $sp, 88", "jr $ra"]);
    assert!(!trampoline.contains("\"bl ") && !trampoline.contains("\"jirl "));

    let save_guest = section(EXCEPTION_ASSEMBLY, ".macro SAVE_GUEST_REGS", ".endm");
    assert!(save_guest.contains("csrrd   $t0, HOST_VCPU_TMP_KS"));
    assert_in_order(save_guest, &["st.d    $r21", "RESTORE_HOST_PERCPU"]);
}
