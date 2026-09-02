//! Source-level checks for the task-context assembly layout contract.
//!
//! The target-specific naked functions cannot execute in host tests. These
//! checks therefore make every memory operand name a Rust-derived offset and
//! require a compile-time layout assertion beside each `TaskContext`.

const X86_CONTEXT: &str = include_str!("../src/x86_64/context.rs");
const AARCH64_CONTEXT: &str = include_str!("../src/aarch64/context.rs");
const RISCV_CONTEXT: &str = include_str!("../src/riscv/context.rs");
const LOONGARCH_CONTEXT: &str = include_str!("../src/loongarch64/context.rs");
const TASK_LOCAL: &str = include_str!("../src/task_local.rs");
const AX_CPU_LIB: &str = include_str!("../src/lib.rs");
const X86_ASM: &str = include_str!("../src/x86_64/asm.rs");
const AARCH64_ASM: &str = include_str!("../src/aarch64/asm.rs");
const RISCV_ASM: &str = include_str!("../src/riscv/asm.rs");
const LOONGARCH_ASM: &str = include_str!("../src/loongarch64/asm.rs");

#[test]
fn task_context_keeps_the_complete_installed_address_space_identity() {
    assert!(
        AX_CPU_LIB.contains("pub struct InstalledAddressSpace")
            && AX_CPU_LIB.contains("space_id: u64")
            && AX_CPU_LIB.contains("tag_generation: u64")
            && AX_CPU_LIB.contains("epoch: u64"),
        "ax-cpu must define the complete software identity installed with a userspace root",
    );

    for (architecture, source) in [
        ("x86_64", X86_CONTEXT),
        ("aarch64", AARCH64_CONTEXT),
        ("riscv", RISCV_CONTEXT),
        ("loongarch64", LOONGARCH_CONTEXT),
    ] {
        let task_context = section(source, "pub struct TaskContext", "impl TaskContext");
        assert!(
            task_context.contains("address_space: InstalledAddressSpace"),
            "{architecture} must retain the address-space id, tag generation and epoch beside the \
             hardware root",
        );
        assert!(
            !task_context.contains("page_table_root") && !source.contains("set_page_table_root"),
            "{architecture} must not let a bare root escape the installed address-space value",
        );
        assert!(
            source.contains("pub fn set_address_space")
                && source.contains("install_user_address_space(_next_ctx.address_space)"),
            "{architecture} must install the complete typed address-space identity only at the \
             architecture switch boundary",
        );
    }
}

#[test]
fn every_architecture_installs_the_typed_tag_at_the_register_boundary() {
    assert!(
        !AX_CPU_LIB.contains("matches!(self.mode, InstalledAddressSpaceMode::FullFlush)"),
        "the common identity validator must not reject a tagged context before the architecture \
         backend can select tagged or full-flush installation",
    );
    for (architecture, context, asm) in [
        ("x86_64", X86_CONTEXT, X86_ASM),
        ("aarch64", AARCH64_CONTEXT, AARCH64_ASM),
        ("riscv", RISCV_CONTEXT, RISCV_ASM),
        ("loongarch64", LOONGARCH_CONTEXT, LOONGARCH_ASM),
    ] {
        let prepare = section(
            context,
            "pub fn prepare_switch_to",
            "pub unsafe fn switch_to_prepared",
        );
        assert!(
            prepare.contains("install_user_address_space(_next_ctx.address_space)"),
            "{architecture} must pass the complete installed identity to its register backend",
        );
        assert!(
            asm.contains("pub fn address_space_tag_capacity")
                && asm.contains("pub unsafe fn install_user_address_space")
                && asm.contains("hardware_tag()"),
            "{architecture} must expose capability probing and typed tag installation",
        );
    }
}

#[test]
fn loongarch_keeps_a_materialized_lazy_root_while_aarch64_allows_kernel_only_identity() {
    let validator = section(
        AX_CPU_LIB,
        "pub(crate) fn validate_architecture_support",
        "impl Default for InstalledAddressSpace",
    );
    let materialized_roots = section(validator, "#[cfg(any(", "#[cfg(target_arch = \"aarch64\")]");
    assert!(
        materialized_roots.contains("target_arch = \"x86_64\"")
            && materialized_roots.contains("target_arch = \"riscv64\"")
            && materialized_roots.contains("target_arch = \"loongarch64\"")
            && materialized_roots.contains("self.root.as_usize() != 0")
            && validator.contains("target_arch = \"aarch64\"")
            && validator.contains("!self.is_user() || self.root.as_usize() != 0"),
        "LoongArch lazy TLB state needs a materialized PGDL like Linux invalid_pg_dir, while \
         AArch64 may use an explicit root-zero kernel identity",
    );
}

#[test]
fn every_task_context_has_a_compile_time_layout_contract() {
    for (architecture, source) in [
        ("x86_64", X86_CONTEXT),
        ("aarch64", AARCH64_CONTEXT),
        ("riscv", RISCV_CONTEXT),
        ("loongarch64", LOONGARCH_CONTEXT),
    ] {
        let task_context = section(source, "pub struct TaskContext", "impl TaskContext");
        assert!(task_context.contains("task_local: TaskLocalState"));
        assert!(
            source.contains("const _: () = {")
                && source.contains("size_of::<KernelTlsBase>() == size_of::<usize>()"),
            "{architecture} must statically prove the word-sized TLS assembly field",
        );
        for cpu_owned_or_address_space in [
            "sscratch:",
            "kernel_gs:",
            "tpidr_el1:",
            "tpidr_el2:",
            "pub r21:",
            "cr3:",
            "satp:",
            "ttbr0_el1:",
            "pgdl:",
        ] {
            assert!(
                !task_context
                    .to_ascii_lowercase()
                    .contains(cpu_owned_or_address_space),
                "{architecture} TaskContext must not own `{cpu_owned_or_address_space}`",
            );
        }
    }
    assert!(TASK_LOCAL.contains("context_header: usize"));
    assert!(TASK_LOCAL.contains("kernel_tls: KernelTlsBase"));
    assert!(TASK_LOCAL.contains("size_of::<TaskLocalState>()"));
}

#[test]
fn x86_switch_uses_only_rust_derived_task_offsets() {
    let context_switch = naked_context_switch(X86_CONTEXT);
    for field in ["rsp", "kernel_tls"] {
        if field == "kernel_tls" {
            assert_task_local_derived_offset(X86_CONTEXT, field, "offset");
        } else {
            assert_rust_derived_offset(X86_CONTEXT, field, "offset");
        }
        assert!(
            context_switch.contains(&format!("{{{field}_offset}}")),
            "x86_64 context switch must use the named `{field}` offset",
        );
    }
    assert!(!context_switch.contains("[rdi]"));
    assert!(!context_switch.contains("[rsi]"));
}

#[test]
fn aarch64_switch_uses_only_rust_derived_task_offsets() {
    let context_switch = naked_context_switch(AARCH64_CONTEXT);
    for field in ["sp", "r19", "r21", "r23", "r25", "r27", "r29", "kernel_tls"] {
        if field == "kernel_tls" {
            assert_task_local_derived_offset(AARCH64_CONTEXT, field, "offset");
        } else {
            assert_rust_derived_offset(AARCH64_CONTEXT, field, "offset");
        }
        assert!(
            context_switch.contains(&format!("{{{field}_offset}}")),
            "AArch64 context switch must use the named `{field}` offset",
        );
    }
    assert_task_local_derived_offset(AARCH64_CONTEXT, "context_header", "offset");
    assert!(AARCH64_CONTEXT.contains("{context_header_offset}"));
    for base in ["x0", "x1"] {
        assert!(!context_switch.contains(&format!("[{base}]")));
        for index in 0..=12 {
            assert!(
                !context_switch.contains(&format!("[{base}, {index} * 8]")),
                "AArch64 context switch must not embed TaskContext slot {index}",
            );
        }
    }
}

#[test]
fn riscv_switch_uses_only_rust_derived_task_offsets() {
    let context_switch = naked_context_switch(RISCV_CONTEXT);
    for field in [
        "ra", "sp", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11",
    ] {
        assert_rust_derived_offset(RISCV_CONTEXT, field, "index");
        assert!(
            context_switch.contains(&format!("{{{field}_index}}")),
            "RISC-V context switch must use the named `{field}` index",
        );
    }
    assert_task_local_derived_offset(RISCV_CONTEXT, "kernel_tls", "index");
    assert_task_local_derived_offset(RISCV_CONTEXT, "context_header", "index");
    assert!(RISCV_CONTEXT.contains("{kernel_tls_index}"));
    assert!(RISCV_CONTEXT.contains("{context_header_index}"));
    assert_no_numeric_macro_slots(context_switch, "a0", 0..=13);
    assert_no_numeric_macro_slots(context_switch, "a1", 0..=13);
}

#[test]
fn loongarch_switch_uses_only_rust_derived_task_offsets() {
    let context_switch = naked_context_switch(LOONGARCH_CONTEXT);
    for field in [
        "ra",
        "sp",
        "s0",
        "s1",
        "s2",
        "s3",
        "s4",
        "s5",
        "s6",
        "s7",
        "s8",
        "frame_pointer",
        "kernel_tls",
    ] {
        assert!(
            context_switch.contains(&format!("{{{field}_offset}}")),
            "LoongArch context switch must use the named `{field}` offset",
        );
    }
    for field in ["ra", "sp"] {
        assert_rust_derived_offset(LOONGARCH_CONTEXT, field, "offset");
    }
    assert_task_local_derived_offset(LOONGARCH_CONTEXT, "kernel_tls", "offset");
    assert_task_local_derived_offset(LOONGARCH_CONTEXT, "context_header", "offset");
    assert!(LOONGARCH_CONTEXT.contains("{context_header_offset}"));
    assert!(
        LOONGARCH_CONTEXT.contains("s0_offset = const offset_of!(TaskContext, s)"),
        "LoongArch saved-register array offsets must derive from its Rust field",
    );
    assert_no_numeric_macro_slots(context_switch, "$a0", 0..=11);
    assert_no_numeric_macro_slots(context_switch, "$a1", 0..=11);
}

fn assert_rust_derived_offset(source: &str, field: &str, suffix: &str) {
    let binding = format!("{field}_{suffix} = const offset_of!(TaskContext, {field})");
    assert!(
        source.contains(&binding),
        "missing Rust-derived assembly binding `{binding}`",
    );
}

fn assert_task_local_derived_offset(source: &str, field: &str, suffix: &str) {
    let binding = format!("{field}_{suffix} = const ");
    let expression = source
        .split_once(&binding)
        .unwrap_or_else(|| panic!("missing task-local assembly binding `{binding}`"))
        .1
        .lines()
        .take(3)
        .collect::<String>();
    assert!(
        expression.contains("offset_of!(TaskContext, task_local)")
            && expression.contains(&format!("offset_of!(TaskLocalState, {field})")),
        "`{binding}` must compose TaskContext and TaskLocalState offsets",
    );
}

fn assert_no_numeric_macro_slots(
    context_switch: &str,
    base: &str,
    slots: impl IntoIterator<Item = usize>,
) {
    for slot in slots {
        assert!(
            !context_switch.contains(&format!(", {base}, {slot}")),
            "context switch must not embed slot {slot} relative to {base}",
        );
    }
}

fn naked_context_switch(source: &str) -> &str {
    section(
        source,
        "unsafe extern \"C\" fn context_switch",
        "\n    )\n}",
    )
}

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
