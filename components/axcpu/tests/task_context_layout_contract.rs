//! Source-level checks for the task-context assembly layout contract.
//!
//! The target-specific naked functions cannot execute in host tests. These
//! checks therefore make every memory operand name a Rust-derived offset and
//! require a compile-time layout assertion beside each `TaskContext`.

const X86_CONTEXT: &str = include_str!("../src/x86_64/context.rs");
const AARCH64_CONTEXT: &str = include_str!("../src/aarch64/context.rs");
const RISCV_CONTEXT: &str = include_str!("../src/riscv/context.rs");
const LOONGARCH_CONTEXT: &str = include_str!("../src/loongarch64/context.rs");

#[test]
fn every_task_context_has_a_compile_time_layout_contract() {
    for (architecture, source) in [
        ("x86_64", X86_CONTEXT),
        ("aarch64", AARCH64_CONTEXT),
        ("riscv", RISCV_CONTEXT),
        ("loongarch64", LOONGARCH_CONTEXT),
    ] {
        let task_context = section(source, "pub struct TaskContext", "impl TaskContext");
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
}

#[test]
fn x86_switch_uses_only_rust_derived_task_offsets() {
    let context_switch = naked_context_switch(X86_CONTEXT);
    for field in ["rsp", "kernel_tls"] {
        assert_rust_derived_offset(X86_CONTEXT, field, "offset");
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
        assert_rust_derived_offset(AARCH64_CONTEXT, field, "offset");
        assert!(
            context_switch.contains(&format!("{{{field}_offset}}")),
            "AArch64 context switch must use the named `{field}` offset",
        );
    }
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
        "ra", "sp", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "tp",
    ] {
        assert_rust_derived_offset(RISCV_CONTEXT, field, "index");
        assert!(
            context_switch.contains(&format!("{{{field}_index}}")),
            "RISC-V context switch must use the named `{field}` index",
        );
    }
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
    for field in ["ra", "sp", "kernel_tls"] {
        assert_rust_derived_offset(LOONGARCH_CONTEXT, field, "offset");
    }
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
