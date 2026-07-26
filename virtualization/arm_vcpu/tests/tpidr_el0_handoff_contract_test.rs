use std::{fs, path::Path};

const HOST_TLS_COOKIE: u64 = 0x484f_5354_5f54_4c53;
const GUEST_TLS_COOKIE: u64 = 0x4755_4553_545f_544c;

#[test]
fn guest_entry_and_exit_keep_host_and_guest_tls_cookies_separate() {
    assert_ne!(HOST_TLS_COOKIE, GUEST_TLS_COOKIE);

    let vcpu = read_source("src/vcpu.rs");
    let context = read_source("src/context_frame.rs");
    let exception = read_source("src/exception.S");
    let exception_rust = read_source("src/exception.rs");

    let host_context = section(
        &vcpu,
        "struct HostRuntimeContext",
        "/// A virtual CPU within a guest.",
    );
    assert!(
        host_context.contains("tpidr_el0: u64"),
        "the assembly-visible host context must retain the host TLS cookie"
    );

    let restore = section(
        &context,
        "    pub unsafe fn restore(&self)",
        "    fn timer_registers(&self)",
    );
    let store = section(
        &context,
        "    pub unsafe fn store(&mut self)",
        "    /// Restores the values",
    );
    assert!(
        !restore.contains("msr TPIDR_EL0"),
        "guest TLS must not become live while Rust restore helpers still execute"
    );
    assert!(
        !store.contains("mrs {0}, TPIDR_EL0"),
        "Rust save helpers run with host TLS and must not overwrite the guest cookie"
    );

    let run_guest = section(
        &vcpu,
        "    unsafe extern \"C\" fn run_guest",
        "    /// This function is called when the control flow comes back",
    );
    assert_in_order(
        run_guest,
        &[
            "mrs x9, tpidr_el0",
            "str x9, [x10, {host_tpidr_el0_delta}]",
            "b context_vm_entry",
        ],
    );
    assert!(
        run_guest.contains("host_tpidr_el0_delta = const"),
        "host TLS assembly must use a Rust-derived field offset"
    );

    let save_vcpu = section(&exception, ".macro SAVE_VCPU_REGS_FROM_EL1", ".endm");
    assert_in_order(
        save_vcpu,
        &[
            "mrs     x9, tpidr_el0",
            "str     x9, [sp, {guest_tpidr_el0_offset}]",
            "ldr     x9, [sp, {host_tpidr_el0_offset}]",
            "msr     tpidr_el0, x9",
        ],
    );
    assert!(
        !save_vcpu.contains("bl      "),
        "VM exit must restore the host cookie before any Rust/helper call"
    );

    let guest_restore = section(&exception, ".macro RESTORE_GUEST_REGS_INTO_EL1", ".endm");
    assert_in_order(
        guest_restore,
        &[
            "ldr     x9, [sp, {guest_tpidr_el0_offset}]",
            "msr     tpidr_el0, x9",
            "ldp     x8, x9, [sp, 8 * 8]",
        ],
    );
    assert!(
        !guest_restore.contains("bl      "),
        "no Rust/helper call may run after the guest cookie becomes live"
    );

    let current_restore = section(&exception, ".macro RESTORE_CURRENT_REGS_INTO_EL2", ".endm");
    assert!(
        !current_restore.contains("tpidr_el0"),
        "a current-EL exception must preserve the already-live host cookie"
    );
    assert!(
        exception.contains("b       .Lexception_return_current_el2")
            && exception.contains("b       .Lexception_return_guest_el1"),
        "current-EL and guest restore paths must remain explicit"
    );

    assert!(
        exception_rust.contains(
            "guest_tpidr_el0_offset = const crate::vcpu::ARM_VCPU_GUEST_TPIDR_EL0_OFFSET",
        ) && exception_rust
            .contains("host_tpidr_el0_offset = const crate::vcpu::ARM_VCPU_HOST_TPIDR_EL0_OFFSET"),
        "global assembly must consume the Rust layout offsets"
    );
    assert!(
        vcpu.contains("offset_of!(HostRuntimeContext, tpidr_el0)")
            && context.contains("offset_of!(GuestSystemRegisters, tpidr_el0)"),
        "both host and guest TLS slots must be derived with offset_of!"
    );
}

fn read_source(relative: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))
        .expect("arm_vcpu source must remain readable")
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .unwrap_or_else(|| panic!("missing section start {start:?}"));
    let tail = &source[start..];
    let end = tail
        .find(end)
        .unwrap_or_else(|| panic!("missing section end {end:?}"));
    &tail[..end]
}

fn assert_in_order(source: &str, patterns: &[&str]) {
    let mut cursor = 0;
    for pattern in patterns {
        let offset = source[cursor..]
            .find(pattern)
            .unwrap_or_else(|| panic!("missing ordered pattern {pattern:?}"));
        cursor += offset + pattern.len();
    }
}
