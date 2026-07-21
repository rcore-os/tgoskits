// SPDX-License-Identifier: Apache-2.0

//! Static checks for the LoongArch register ownership contract.
//!
//! These tests intentionally inspect the assembly sources on every host
//! architecture. Runtime coverage still belongs to the LoongArch SMP/QEMU
//! suite, but source-level checks prevent the scratch-register allocation and
//! trap-return rules from silently drifting. The vCPU crate carries the
//! corresponding host-register boundary check.

const TRAP_MACROS: &str = include_str!("../src/loongarch64/macros.rs");
const TRAP_ENTRY: &str = include_str!("../src/loongarch64/trap.S");
const TASK_CONTEXT: &str = include_str!("../src/loongarch64/context.rs");
const ARCH_ASM: &str = include_str!("../src/loongarch64/asm.rs");

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
fn trap_scratch_registers_have_one_named_allocation() {
    for allocation in [
        ".equ KSAVE_KSP,            0x30",
        ".equ KSAVE_T0,             0x31",
        ".equ KSAVE_T1,             0x32",
        ".equ KSAVE_PERCPU,         0x33",
        ".equ KSAVE_VCPU,           0x34",
        ".equ KSAVE_VCPU_TMP,       0x35",
    ] {
        assert!(
            TRAP_MACROS.contains(allocation),
            "missing LoongArch trap allocation `{allocation}`"
        );
    }
    assert!(
        TRAP_MACROS.contains("cpu_local::loongarch64"),
        "the assembly allocation must be compile-time checked against cpu-local"
    );
}

#[test]
fn trap_entry_separates_kernel_percpu_from_user_u0() {
    assert!(
        TRAP_ENTRY.contains("RESTORE_KERNEL_GENERAL_REGS"),
        "kernel trap return needs an origin-specific restore macro"
    );
    assert!(
        TRAP_ENTRY.contains("RESTORE_USER_GENERAL_REGS"),
        "user trap return needs an origin-specific restore macro"
    );

    let saved_frame = section(TRAP_ENTRY, "PUSH_GENERAL_REGS", "move    $a0, $sp");
    assert!(
        saved_frame.contains("csrrd   $r21, KSAVE_PERCPU"),
        "user-origin traps must recover the kernel per-CPU base from KS3 after saving user u0"
    );
    assert_in_order(
        saved_frame,
        "PUSH_GENERAL_REGS",
        "csrrd   $r21, KSAVE_PERCPU",
    );

    let exit_user = section(TRAP_ENTRY, ".Lexit_user:", ".global enter_user");
    assert!(
        !exit_user.contains("LDD     $r21"),
        "returning from a user trap to kernel Rust must not restore a saved kernel r21"
    );
}

#[test]
fn kernel_trap_initializes_the_complete_typed_register_frame() {
    let kernel_entry = section(
        TRAP_ENTRY,
        "    bnez    $t1, .Ltrap_from_user",
        ".Ltrap_from_user:",
    );
    assert!(
        kernel_entry.contains("STD     $r0, $sp, 0"),
        "the skipped r0 slot must be initialized before Rust borrows the whole trap frame"
    );
    assert_in_order(
        kernel_entry,
        "addi.d  $sp, $sp, -{trapframe_size}",
        "STD     $r0, $sp, 0",
    );

    let user_entry = section(TRAP_ENTRY, ".Ltrap_from_user:", ".Ltrap_stack_ready:");
    assert!(
        !user_entry.contains("STD     $r0, $sp, 0"),
        "the user path temporarily owns slot zero as its kernel continuation"
    );
}

#[test]
fn task_switch_owns_tls_but_never_percpu() {
    let task_context_definition = section(
        TASK_CONTEXT,
        "pub struct TaskContext",
        "impl Default for TaskContext",
    );
    assert!(
        task_context_definition.contains("KernelTlsBase"),
        "TaskContext must use the architecture-neutral kernel TLS newtype"
    );
    assert!(
        !task_context_definition.contains("pub tp: usize"),
        "the raw task TLS register must not remain a public usize field"
    );
    assert!(
        !TASK_CONTEXT.contains("crate::asm::write_thread_pointer(next_ctx.tp)"),
        "tp must not change in Rust before the final context-switch boundary"
    );

    let context_switch = section(
        TASK_CONTEXT,
        "unsafe extern \"C\" fn context_switch",
        "ret\",",
    );
    assert!(
        context_switch.contains("st.d    $tp, $a0, {kernel_tls_offset}")
            && context_switch.contains("ld.d    $tp, $a1, {kernel_tls_offset}"),
        "kernel task TLS must switch beside the callee-saved registers"
    );
    assert!(
        TASK_CONTEXT.contains("kernel_tls_offset = const offset_of!(TaskContext, kernel_tls)"),
        "the assembly boundary must derive the TLS offset from TaskContext"
    );
    assert!(
        !context_switch.contains("STD     $r21")
            && !context_switch.contains("LDD     $r21")
            && !context_switch.contains("st.d    $r21")
            && !context_switch.contains("ld.d    $r21"),
        "the CPU-local base must never become task context"
    );
}

#[test]
fn current_scheduler_installs_address_space_before_the_raw_switch() {
    let prepare = section(
        TASK_CONTEXT,
        "pub fn prepare_switch_to(&mut self, _next_ctx: &Self)",
        "pub unsafe fn switch_to_raw",
    );
    assert!(
        TASK_CONTEXT.contains("page_table_root: usize")
            && TASK_CONTEXT.contains("pub fn set_page_table_root")
            && prepare.contains("write_user_page_table")
            && prepare.contains("flush_tlb"),
        "the existing axtask model must retain task-owned address-space selection"
    );

    let raw_switch = section(
        TASK_CONTEXT,
        "unsafe extern \"C\" fn context_switch_raw",
        "ret\",",
    );
    assert!(
        !raw_switch.contains("write_user_page_table") && !raw_switch.contains("flush_tlb"),
        "fallible or policy-bearing address-space work must precede current-register publication"
    );
}

#[test]
fn raw_tp_access_is_typed_as_task_tls() {
    assert!(
        ARCH_ASM
            .contains("#[cfg(feature = \"tls\")]\npub fn read_thread_pointer() -> KernelTlsBase"),
        "reading kernel tp must return task-owned TLS state"
    );
    assert!(
        ARCH_ASM.contains(
            "#[cfg(feature = \"tls\")]\npub unsafe fn write_thread_pointer(kernel_tls: \
             KernelTlsBase)"
        ),
        "writing kernel tp must require task-owned TLS state"
    );
    assert!(
        !ARCH_ASM.contains("thread pointer of the current CPU"),
        "tp documentation must not describe task state as CPU-local state"
    );
}
