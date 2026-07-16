// SPDX-License-Identifier: Apache-2.0

//! Static checks for the host registers used by LoongArch virtualization.

const VCPU_ENTRY: &str = include_str!("../src/exception.S");
const VCPU_TRAMPOLINE: &str = include_str!("../src/exception.rs");
const VCPU_RUN: &str = include_str!("../src/vcpu.rs");
const PER_CPU: &str = include_str!("../src/pcpu.rs");
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
fn vcpu_uses_kvm_scratch_without_clobbering_percpu_shadow() {
    assert!(
        MANIFEST.contains("ax-cpu-local.workspace = true"),
        "the vCPU must bind its KS allocation to ax-cpu-local"
    );
    assert!(
        VCPU_TRAMPOLINE.contains("ax_cpu_local::loongarch64"),
        "the assembly allocation must be compile-time checked against ax-cpu-local"
    );
    for allocation in [
        ".equ HOST_TRAP_KSP_KS, 0x30",
        ".equ HOST_TRAP_T0_KS,  0x31",
        ".equ HOST_TRAP_T1_KS,  0x32",
        ".equ HOST_PERCPU_KS,  0x33",
        ".equ HOST_VCPU_KS,   0x34",
        ".equ HOST_VCPU_TMP_KS, 0x35",
    ] {
        assert!(
            VCPU_ENTRY.contains(allocation),
            "missing LoongArch vCPU allocation `{allocation}`"
        );
    }

    assert!(
        VCPU_ENTRY.contains("csrwr   $t0, HOST_VCPU_KS"),
        "guest entry must publish its host-stack anchor in KS4"
    );
    assert!(
        VCPU_ENTRY.contains("csrrd   $sp, HOST_VCPU_KS"),
        "guest exits must recover their host-stack anchor from KS4"
    );
    assert!(
        !VCPU_ENTRY.contains("csrwr   $t0, 0x33") && !VCPU_ENTRY.contains("csrrd   $sp, 0x33"),
        "KS3 is reserved for the host per-CPU shadow"
    );

    let save_guest = section(VCPU_ENTRY, ".macro SAVE_GUEST_REGS", ".endm");
    assert_in_order(save_guest, "st.d    $r21", "RESTORE_HOST_PERCPU");
    assert_eq!(
        VCPU_ENTRY.matches("\n    SAVE_GUEST_REGS\n").count(),
        4,
        "all four guest-exit vectors must use the register-save contract"
    );

    assert!(
        !VCPU_RUN.contains("st.d $r21") && !VCPU_TRAMPOLINE.contains("ld.d $r21"),
        "host r21 is CPU-owned and must not be saved as vCPU task context"
    );
}

#[test]
fn vm_exit_restores_host_tls_before_returning_to_rust() {
    let trampoline = section(
        VCPU_TRAMPOLINE,
        "unsafe extern \"C\" fn vmexit_trampoline",
        "ctx_size = const core::mem::size_of::<LoongArchContextFrame>()",
    );

    assert_in_order(trampoline, "ld.d $tp, $sp, 88", "jr $ra");
    assert!(
        !trampoline.contains("\"bl ") && !trampoline.contains("\"jirl "),
        "the trampoline must not call Rust or helpers before returning with host tp"
    );

    let save_guest = section(VCPU_ENTRY, ".macro SAVE_GUEST_REGS", ".endm");
    assert_in_order(save_guest, "st.d    $r21", "RESTORE_HOST_PERCPU");
    assert!(
        save_guest.contains("csrrd   $t0, HOST_VCPU_TMP_KS"),
        "guest t0 must be recovered from KS5 before the host anchor is restored"
    );
}

#[test]
fn cpu_owned_gintc_and_full_ecfg_live_at_pinned_boundaries() {
    let setup = section(VCPU_RUN, "pub fn setup", "pub fn run");
    assert!(
        !setup.contains("gintc_set_hwi_passthrough"),
        "unbound vCPU setup must not modify CPU-owned GINTC state"
    );
    assert!(
        PER_CPU.contains("gintc_set_hwi_passthrough(0)"),
        "GINTC host policy must be installed once by pinned per-CPU enable"
    );

    let unbind = section(VCPU_RUN, "pub fn unbind", "pub fn set_gpr");
    assert!(
        !unbind.contains("set_ecfg_line_enabled"),
        "unbind must restore the saved host ECFG without forcing the timer line on"
    );
    assert!(
        VCPU_RUN.contains("ecfg: usize") && VCPU_RUN.contains("csr_write::<CSR_ECFG>(state.ecfg)"),
        "bind/unbind must preserve the complete host ECFG value"
    );
}

#[test]
fn live_csr_operations_require_a_cpu_pin() {
    for operation in ["run", "bind", "unbind"] {
        let signature = VCPU_RUN
            .split_once(&format!("pub fn {operation}"))
            .unwrap_or_else(|| panic!("missing LoongArchVcpu::{operation}"))
            .1
            .split_once('{')
            .expect("vCPU operation must have a body")
            .0;
        assert!(
            signature.contains("&CpuPin"),
            "LoongArchVcpu::{operation} must require a borrowed CPU pin"
        );
    }

    for forbidden in [
        "pub fn set_hwi_interrupts(mask: usize)",
        "pub fn inject_interrupt(vector: usize)",
    ] {
        assert!(
            !include_str!("../src/registers.rs").contains(forbidden),
            "live LoongArch CSR mutation must not be safely public without a CPU pin: {forbidden}"
        );
    }
}
