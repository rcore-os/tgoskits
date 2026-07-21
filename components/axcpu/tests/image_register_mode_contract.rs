const MANIFEST: &str = include_str!("../Cargo.toml");
const LIB: &str = include_str!("../src/lib.rs");
const RISCV_CONTEXT: &str = include_str!("../src/riscv/context.rs");
const RISCV_TRAP: &str = include_str!("../src/riscv/trap.S");
const RISCV_TLS_TRAP: &str = include_str!("../src/riscv/trap_tls.S");
const AARCH_CONTEXT: &str = include_str!("../src/aarch64/context.rs");
const AARCH_TRAP: &str = include_str!("../src/aarch64/trap.S");
const X86_CONTEXT: &str = include_str!("../src/x86_64/context.rs");
const LOONGARCH_CONTEXT: &str = include_str!("../src/loongarch64/context.rs");

#[test]
fn tls_feature_selects_register_semantics_without_changing_context_layout() {
    assert!(MANIFEST.contains("tls = [\"cpu-local/tls\"]"));
    for source in [RISCV_CONTEXT, AARCH_CONTEXT, X86_CONTEXT, LOONGARCH_CONTEXT] {
        let task_context = source
            .split_once("pub struct TaskContext")
            .expect("TaskContext must exist")
            .1
            .split_once("\n}")
            .expect("TaskContext must have a bounded layout")
            .0;
        assert!(task_context.contains("current_header"));
        assert!(task_context.contains("kernel_tls"));
        assert!(
            !task_context.contains("cfg(feature = \"tls\")"),
            "image mode must not change TaskContext ABI"
        );
    }
    assert!(LIB.contains("fn for_task_context"));
    assert!(LIB.contains("requested.0 == 0"));
    assert!(LIB.contains("cfg(all(feature = \"uspace\", feature = \"tls\"))"));
}

#[test]
fn riscv_linux_current_trap_uses_the_user_tp_sscratch_handshake() {
    for instruction in [
        "csrrw   tp, sscratch, tp",
        "bnez    tp, .Ltrap_from_user",
        "csrw    sscratch, zero",
        "lla     gp, __global_pointer$",
    ] {
        assert!(
            RISCV_TRAP.contains(instruction),
            "RISC-V LinuxCurrent trap boundary is missing `{instruction}`"
        );
    }
    assert!(RISCV_TRAP.contains("RESTORE_USER_CONTEXT"));
    assert!(RISCV_TRAP.contains("RESTORE_KERNEL_CONTEXT"));
    assert!(!RISCV_TRAP.contains("POP_GENERAL_REGS"));

    let user = section(RISCV_TRAP, ".Ltrap_from_user:", ".Ltrap_saved:");
    assert_in_order(user, "STR     t1, sp, 4", "csrw    sscratch, zero");
    let rust_dispatch = section(RISCV_TRAP, ".Ltrap_saved:", "j       riscv_trap_handler");
    assert_in_order(
        rust_dispatch,
        "csrw    sscratch, zero",
        "lla     gp, __global_pointer$",
    );
    assert_in_order(rust_dispatch, "lla     gp", "mv      a0, sp");

    assert!(RISCV_TLS_TRAP.contains("csrrw   t0, sscratch, t0"));
    assert!(!RISCV_TLS_TRAP.contains("csrrw   tp, sscratch, tp"));
}

#[test]
fn context_switches_select_tls_only_for_unikernel_images() {
    for source in [RISCV_CONTEXT, AARCH_CONTEXT, X86_CONTEXT, LOONGARCH_CONTEXT] {
        assert!(source.contains("#[cfg(feature = \"tls\")]"));
        assert!(source.contains("#[cfg(not(feature = \"tls\"))]"));
        assert!(source.contains("pub fn prepare_switch_to("));
        assert!(source.contains("pub unsafe fn switch_to_raw("));
        assert!(!source.contains("pub fn switch_to(&mut self, next_ctx: &Self)"));
    }
}

#[test]
fn aarch64_user_exit_restores_linux_current_before_rust() {
    let exit = section(AARCH_TRAP, ".Lexit_user:", ".global enter_user");
    assert_in_order(exit, "stp     x8, x9", "mrs     x12, tpidr_el1");
    assert_in_order(
        exit,
        "mrs     x12, tpidr_el1",
        "ldr     x12, [x12, {current_thread_offset}]",
    );
    assert_in_order(
        exit,
        "ldr     x12, [x12, {current_thread_offset}]",
        "msr     sp_el0, x12",
    );
    assert_in_order(exit, "msr     sp_el0, x12", "\n    ret");

    let enter = section(AARCH_TRAP, "enter_user:", ".Lexception_return:");
    assert_in_order(enter, "ldp     x8, x9", "msr     sp_el0, x8");
    assert_in_order(enter, "msr     sp_el0, x8", "msr     tpidr_el0, x9");
    assert_in_order(enter, "msr     tpidr_el0, x9", "mov     sp, x0");
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let tail = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start `{start}`"))
        .1;
    tail.split_once(end)
        .unwrap_or_else(|| panic!("missing section end `{end}`"))
        .0
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
