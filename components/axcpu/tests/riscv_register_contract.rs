// SPDX-License-Identifier: Apache-2.0

//! Static checks for the RISC-V register ownership contract.
//!
//! These tests inspect target assembly on every host architecture. Runtime
//! coverage still belongs to the RISC-V SMP/QEMU suites, but source-level
//! checks prevent CPU-local, TLS, and boot-only register roles from silently
//! drifting across `ax-cpu`, `riscv_vcpu`, and `someboot`.

const CONTEXT: &str = include_str!("../src/riscv/context.rs");
const TASK_LOCAL: &str = include_str!("../src/task_local.rs");
const ASM: &str = include_str!("../src/riscv/asm.rs");
const TRAP_ENTRY: &str = include_str!("../src/riscv/trap.S");
const TLS_TRAP_ENTRY: &str = include_str!("../src/riscv/trap_tls.S");
const TRAP_GLUE: &str = include_str!("../src/riscv/trap.rs");
const LOCAL_STATE: &str = include_str!("../src/riscv/local_state.rs");
const VCPU_DETECT: &str = include_str!("../../../virtualization/riscv_vcpu/src/detect.rs");
const SOMEBOOT_ENTRY: &str = include_str!("../../../platforms/someboot/src/arch/riscv64/entry.rs");
const SOMEBOOT_ARCH: &str = include_str!("../../../platforms/someboot/src/arch/riscv64/mod.rs");
const SOMEBOOT_PAGING: &str =
    include_str!("../../../platforms/someboot/src/arch/riscv64/paging.rs");

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
fn task_context_switches_tp_inside_the_naked_handoff() {
    let prepare = section(
        CONTEXT,
        "    pub fn prepare_switch_to(&mut self, _next_ctx: &Self) {",
        "    /// Performs only the final GPR/current/TLS transfer.",
    );
    assert!(
        !prepare.contains("write_thread_pointer"),
        "Rust must not install next tp while the previous stack is still active"
    );
    assert!(!CONTEXT.contains("pub fn switch_to(&mut self, next_ctx: &Self)"));

    let tls_switch = section(
        CONTEXT,
        "#[cfg(feature = \"tls\")]\n#[unsafe(naked)]",
        "#[cfg(not(feature = \"tls\"))]",
    );
    assert!(
        tls_switch.contains("STR     tp, a0, {kernel_tls_index}"),
        "UnikernelTls must save previous task TLS"
    );
    assert!(
        tls_switch.contains("LDR     tp, a1, {kernel_tls_index}"),
        "UnikernelTls must restore next task TLS"
    );
    let linux_switch = section(
        CONTEXT,
        "#[cfg(not(feature = \"tls\"))]\n#[unsafe(naked)]",
        "+ offset_of!(TaskLocalState, current_header)) / size_of::<usize>(),",
    );
    assert!(!linux_switch.contains("STR     tp"));
    assert!(linux_switch.contains("LDR     tp, a1, {current_header_index}"));
    assert_in_order(linux_switch, "LDR     sp", "LDR     tp");
    assert_in_order(linux_switch, "LDR     tp", "\n        ret\"");
}

#[test]
fn fp_switch_enables_hardware_before_restoring_registers() {
    let fp_switch = section(
        CONTEXT,
        "pub fn switch_to(&mut self, next_fp_state: &FpState)",
        "/// Saved registers when a trap",
    );

    assert!(
        fp_switch.contains("sstatus::set_fs(FS::Dirty)"),
        "an FS=Off bootstrap context must enable FP before the first restore instruction"
    );
    assert_in_order(
        fp_switch,
        "sstatus::set_fs(FS::Dirty)",
        "next_fp_state.restore()",
    );
    assert_in_order(fp_switch, "sstatus::set_fs(FS::Dirty)", "FpState::clear()");
    assert_in_order(
        fp_switch,
        "FpState::clear()",
        "sstatus::set_fs(next_fp_state.fs)",
    );
}

#[test]
fn task_context_owns_kernel_tls_and_preserves_current_address_space_model() {
    let task_fields = section(CONTEXT, "pub struct TaskContext", "impl TaskContext");
    assert!(
        CONTEXT.contains("tls_area: KernelTlsBase"),
        "TaskContext::init must distinguish kernel TLS from an arbitrary address"
    );
    assert!(
        CONTEXT.contains("kernel_tls_index = const (offset_of!(TaskContext, task_local)")
            && CONTEXT.contains("offset_of!(TaskLocalState, kernel_tls)")
            && CONTEXT
                .contains("current_header_index = const (offset_of!(TaskContext, task_local)")
            && CONTEXT.contains("offset_of!(TaskLocalState, current_header)")
            && TASK_LOCAL.contains("kernel_tls: KernelTlsBase")
            && TASK_LOCAL.contains("current_header: usize"),
        "both image-mode assembly offsets must derive from TaskContext"
    );

    for forbidden in ["pub tp: usize", "pub satp:"] {
        assert!(
            !task_fields.contains(forbidden),
            "TaskContext must not contain register operation `{forbidden}`"
        );
    }
    let prepare = section(
        CONTEXT,
        "pub fn prepare_switch_to(&mut self, _next_ctx: &Self)",
        "pub unsafe fn switch_to_prepared",
    );
    assert!(
        task_fields.contains("page_table_root: ax_memory_addr::PhysAddr")
            && CONTEXT.contains("pub fn set_page_table_root")
            && prepare.contains("write_user_page_table")
            && prepare.contains("flush_tlb"),
        "the existing axtask model must retain task-owned address-space selection"
    );

    let raw_switches = CONTEXT
        .split_once("unsafe extern \"C\" fn context_switch_raw")
        .expect("raw context switch")
        .1;
    assert!(
        !raw_switches.contains("write_user_page_table") && !raw_switches.contains("flush_tlb"),
        "address-space work must complete before current-register publication and raw transfer"
    );
}

#[test]
fn thread_pointer_api_exposes_task_owned_kernel_tls() {
    assert!(
        ASM.contains("#[cfg(feature = \"tls\")]\npub fn read_thread_pointer() -> KernelTlsBase"),
        "reading tp must preserve the task-owned kernel TLS type"
    );
    assert!(
        ASM.contains(
            "#[cfg(feature = \"tls\")]\npub unsafe fn write_thread_pointer(tls_base: \
             KernelTlsBase)"
        ),
        "writing tp must not accept an untyped CPU-local or arbitrary address"
    );
    assert!(
        ASM.contains("task-owned kernel TLS"),
        "tp documentation must state its ownership contract"
    );
}

#[test]
fn trap_entry_uses_privilege_origin_and_restores_cpu_anchor_before_rust() {
    assert!(
        TRAP_ENTRY.contains("csrrw   tp, sscratch, tp")
            && TRAP_ENTRY.contains("bnez    tp, .Ltrap_from_user"),
        "LinuxCurrent must use the tp/sscratch origin handshake"
    );
    assert!(
        TRAP_ENTRY.contains("csrw    sscratch, zero"),
        "kernel Rust must observe canonical sscratch=0"
    );
    assert!(
        TRAP_ENTRY.contains("__global_pointer$"),
        "kernel gp must be restored to the standard RISC-V global pointer"
    );
    assert!(
        !TRAP_ENTRY.contains("csrrw   t0, sscratch, t0"),
        "LinuxCurrent must not treat sscratch as the permanent CPU prefix"
    );

    assert!(TRAP_GLUE.contains("CURRENT_THREAD_CPU_BASE_OFFSET"));
    for field in [
        "CPU_KERNEL_STACK_POINTER_OFFSET",
        "CPU_USER_TRAP_FRAME_OFFSET",
        "THREAD_SCRATCH0_OFFSET",
        "THREAD_SCRATCH1_OFFSET",
    ] {
        assert!(
            TRAP_GLUE.contains(field),
            "trap glue must bind the RISC-V local-state field `{field}`"
        );
    }
    for field in [
        "struct CpuTrapState",
        "struct ThreadTrapState",
        "CPU_AREA_ARCH_STATE_OFFSET",
        "CURRENT_THREAD_ARCH_STATE_OFFSET",
    ] {
        assert!(
            LOCAL_STATE.contains(field),
            "RISC-V local state must own `{field}`"
        );
    }

    let kernel_dispatch = section(
        TRAP_ENTRY,
        "trap_vector_base:",
        "j       riscv_trap_handler",
    );
    assert_in_order(kernel_dispatch, "csrw    sscratch, zero", "mv      a0, sp");
    assert_in_order(kernel_dispatch, "__global_pointer$", "mv      a0, sp");

    let enter_user = section(TRAP_ENTRY, "enter_user:", ".Ltrap_return:");
    assert!(
        enter_user.contains("{kernel_stack_pointer_index}")
            && enter_user.contains("{user_trap_frame_index}"),
        "user entry must publish both sides of the trap stack handoff in the CPU area"
    );
    assert!(
        TRAP_GLUE.contains("unsafe extern \"C\" fn riscv_trap_handler(raw_tf: *mut RawTrapFrame)"),
        "assembly must enter Rust through an explicit raw C ABI boundary"
    );
}

#[test]
fn kernel_trap_initializes_the_complete_typed_register_frame() {
    let kernel_entry = section(
        TRAP_ENTRY,
        "    // Kernel trap: recover the interrupted current header and return",
        ".Ltrap_from_user:",
    );
    assert!(
        kernel_entry.contains("STR     zero, sp, 0"),
        "the skipped x0 slot must be initialized before Rust borrows the whole trap frame"
    );
    assert_in_order(
        kernel_entry,
        "addi    sp, sp, -{trapframe_size}",
        "STR     zero, sp, 0",
    );
    assert_in_order(
        kernel_entry,
        "STR     zero, sp, 0",
        "\n    PUSH_GENERAL_REGS\n",
    );
}

#[test]
fn tls_kernel_trap_initializes_the_complete_typed_register_frame() {
    let kernel_entry = section(
        TLS_TRAP_ENTRY,
        "    addi    sp, sp, -{trapframe_size}",
        ".Ltrap_from_user:",
    );
    assert!(
        kernel_entry.contains("STR     zero, sp, 0"),
        "the TLS trap path must initialize the skipped x0 slot before Rust borrows the frame"
    );
    assert_in_order(
        kernel_entry,
        "addi    sp, sp, -{trapframe_size}",
        "STR     zero, sp, 0",
    );
    assert_in_order(
        kernel_entry,
        "STR     zero, sp, 0",
        "\n    PUSH_GENERAL_REGS\n",
    );
}

#[test]
fn vcpu_extension_probe_never_repurposes_thread_pointer() {
    for forbidden in ["trap_frame.tp", "mv  {}, tp", "mv  tp, {}"] {
        assert!(
            !VCPU_DETECT.contains(forbidden),
            "vCPU extension detection must not use kernel TLS register pattern `{forbidden}`"
        );
    }
    assert!(
        VCPU_DETECT.contains("DetectState"),
        "probe result and temporary trap state must live in explicit memory"
    );
    assert!(
        VCPU_DETECT.contains("assert_eq!(")
            && VCPU_DETECT.contains("trap_addr & 0b11,")
            && VCPU_DETECT.contains("H-extension probe trap vector must be four-byte aligned"),
        "stvec must be proven four-byte aligned instead of adjusted heuristically"
    );
}

#[test]
fn someboot_does_not_carry_hart_id_in_thread_pointer() {
    assert!(
        !SOMEBOOT_ENTRY.contains("mv tp, a0"),
        "entry assembly must not carry the boot hart ID in the TLS register"
    );
    assert!(
        !SOMEBOOT_ARCH.contains("mv {hart_id}, tp"),
        "shared Rust boot code must not recover hart ID from the TLS register"
    );
    assert!(
        SOMEBOOT_ENTRY.contains("__global_pointer$"),
        "someboot must establish the standard global pointer before shared Rust"
    );
    assert!(
        SOMEBOOT_ENTRY.contains("fn mmu_entry_rust()")
            && SOMEBOOT_ENTRY.contains("fn secondary_mmu_entry"),
        "both MMU transitions need a high-address assembly boundary"
    );
    assert!(
        SOMEBOOT_PAGING.contains("secondary_mmu_entry") && SOMEBOOT_PAGING.contains("v_trampoline"),
        "secondary MMU enable must jump through the gp-restoring trampoline"
    );
}
