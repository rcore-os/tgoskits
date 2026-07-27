// SPDX-License-Identifier: Apache-2.0

//! Static checks for CPU-local registers used by LoongArch virtualization.

const VCPU_ENTRY: &str = include_str!("../src/exception.S");
const VCPU_TRAMPOLINE: &str = include_str!("../src/exception.rs");
const VCPU_RUN: &str = include_str!("../src/vcpu.rs");
const MANIFEST: &str = include_str!("../Cargo.toml");

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

fn assert_in_order(source: &str, earlier: &str, later: &str) {
    let earlier = source
        .find(earlier)
        .unwrap_or_else(|| panic!("missing `{earlier}`"));
    let later = source
        .find(later)
        .unwrap_or_else(|| panic!("missing `{later}`"));
    assert!(earlier < later, "`{earlier}` must precede `{later}`");
}

#[test]
fn vcpu_uses_ks4_and_ks5_without_clobbering_the_ks3_percpu_shadow() {
    assert!(MANIFEST.contains("cpu-local.workspace = true"));
    assert!(VCPU_TRAMPOLINE.contains("cpu_local::loongarch64"));
    for allocation in [
        ".equ HOST_TRAP_KSP_KS, 0x30",
        ".equ HOST_TRAP_T0_KS,  0x31",
        ".equ HOST_TRAP_T1_KS,  0x32",
        ".equ HOST_PERCPU_KS,  0x33",
        ".equ HOST_VCPU_KS,   0x34",
        ".equ HOST_VCPU_TMP_KS, 0x35",
    ] {
        assert!(VCPU_ENTRY.contains(allocation), "missing `{allocation}`");
    }

    assert!(VCPU_ENTRY.contains("csrwr   $t0, HOST_VCPU_KS"));
    assert!(VCPU_ENTRY.contains("csrrd   $sp, HOST_VCPU_KS"));
    assert!(!VCPU_ENTRY.contains("csrwr   $t0, 0x33"));
    assert!(!VCPU_ENTRY.contains("csrrd   $sp, 0x33"));

    let save_guest = section(VCPU_ENTRY, ".macro SAVE_GUEST_REGS", ".endm");
    assert_in_order(save_guest, "st.d    $r21", "RESTORE_HOST_PERCPU");
    assert_eq!(VCPU_ENTRY.matches("\n    SAVE_GUEST_REGS\n").count(), 4);
    assert!(!VCPU_RUN.contains("st.d $r21"));
    assert!(!VCPU_TRAMPOLINE.contains("ld.d $r21"));
}

#[test]
fn vm_exit_restores_host_tls_before_returning_to_rust() {
    let trampoline = section(
        VCPU_TRAMPOLINE,
        "unsafe extern \"C\" fn vmexit_trampoline",
        "ctx_size = const core::mem::size_of::<LoongArchContextFrame>()",
    );
    assert_in_order(trampoline, "ld.d $tp, $sp, 88", "jr $ra");
    assert!(!trampoline.contains("\"bl ") && !trampoline.contains("\"jirl "));

    let save_guest = section(VCPU_ENTRY, ".macro SAVE_GUEST_REGS", ".endm");
    assert!(save_guest.contains("csrrd   $t0, HOST_VCPU_TMP_KS"));
    assert_in_order(save_guest, "st.d    $r21", "RESTORE_HOST_PERCPU");
}
