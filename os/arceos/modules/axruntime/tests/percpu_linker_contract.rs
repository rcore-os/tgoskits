//! Source-level contract for the fixed per-CPU area prefix.

const SOMEBOOT_DATA: &str = include_str!("../../../../../platforms/someboot/src/ld/data.ld");
const SOMEBOOT_LINKER: &str =
    include_str!("../../../../../platforms/someboot/build_support/linker.rs");
const SOMEBOOT_SMP: &str = include_str!("../../../../../platforms/someboot/src/smp/mod.rs");
const PERCPU_INITIALIZATION: &str =
    include_str!("../../../../../components/percpu/percpu/src/initialization.rs");
const AARCH64_LINKER: &str =
    include_str!("../../../../../platforms/someboot/src/arch/aarch64/link.ld");
const LOONGARCH64_LINKER: &str =
    include_str!("../../../../../platforms/someboot/src/arch/loongarch64/link.ld");
const RISCV64_LINKER: &str =
    include_str!("../../../../../platforms/someboot/src/arch/riscv64/link.ld");
const X86_64_LINKER: &str =
    include_str!("../../../../../platforms/someboot/src/arch/x86_64/link.ld");
const SCOPE_LOCAL_LINKER: &str = include_str!("../../../../../components/scope-local/percpu.x");
const DYNAMIC_LINKER: &str = include_str!("../../../../../platforms/axplat-dyn/link.ld");
const X86_TRAP: &str = include_str!("../../../../../components/axcpu/src/x86_64/trap.S");

fn assert_prefix_is_first(script: &str, name: &str) {
    let prefix = script
        .find("KEEP(*(.percpu.000.header))")
        .unwrap_or_else(|| panic!("{name} must retain the fixed per-CPU prefix"));
    let remaining = script
        .find("SORT_BY_NAME(.percpu.*)")
        .unwrap_or_else(|| panic!("{name} must order the remaining per-CPU sections"));

    assert!(
        prefix < remaining,
        "{name} must place the fixed prefix before every generated per-CPU variable",
    );
    assert!(
        script.contains("ASSERT(__AX_CPU_AREA_PREFIX == _percpu_load_start")
            || script.contains("ASSERT(__AX_CPU_AREA_PREFIX == __percpu_start"),
        "{name} must reject a link whose prefix is not at template offset zero",
    );
}

#[test]
fn production_link_chain_retains_init_records_and_places_the_prefix_at_offset_zero() {
    assert_prefix_is_first(SOMEBOOT_DATA, "someboot data template");
    assert_prefix_is_first(SCOPE_LOCAL_LINKER, "scope-local linker script");

    assert!(SOMEBOOT_DATA.contains("KEEP(*(.ax_percpu.init))"));
    assert!(SOMEBOOT_DATA.contains("__AX_PERCPU_INIT_START"));
    assert!(SOMEBOOT_DATA.contains("__AX_PERCPU_INIT_END"));
    assert!(
        SOMEBOOT_LINKER
            .contains(r#"(DATA_PATH, "${ld_data}", include_str!("../src/ld/data.ld"))"#,),
        "someboot must render the retained data fragment into the final linker script",
    );
    for (name, linker) in [
        ("aarch64", AARCH64_LINKER),
        ("loongarch64", LOONGARCH64_LINKER),
        ("riscv64", RISCV64_LINKER),
        ("x86_64", X86_64_LINKER),
    ] {
        assert!(
            linker.contains("${ld_data}"),
            "{name} production linker chain must include the shared data fragment",
        );
    }
}

#[test]
fn final_high_initialization_uses_relative_descriptors_not_template_copying() {
    assert!(
        PERCPU_INITIALIZATION.contains("storage_address.checked_sub(template_base)"),
        "loaded descriptor addresses must be reduced to template-relative offsets",
    );
    assert!(
        SOMEBOOT_SMP.contains("pub(crate) fn initialize_percpu_layout()")
            && SOMEBOOT_SMP.contains("__ax_percpu_initialize_layout_v2(")
            && SOMEBOOT_SMP.contains("let runtime_base =")
            && SOMEBOOT_SMP.contains("percpu_data_ptr(0)"),
        "someboot must construct the typed layout from its final-high runtime mapping",
    );
    assert!(
        !SOMEBOOT_SMP.contains("copy_nonoverlapping"),
        "boot must not copy a stale linked template into live CPU areas",
    );
}

#[test]
fn x86_trap_uses_a_template_offset_with_an_area_base_gs_anchor() {
    assert!(X86_TRAP.contains("offset __PERCPU_TSS_OFFSET"));
    assert!(
        !X86_TRAP.contains("offset __PERCPU_TSS +"),
        "a runtime-area GS base must not add the absolute template VMA",
    );
    assert!(SOMEBOOT_DATA.contains("__PERCPU_TSS_OFFSET = __PERCPU_TSS - __percpu_start"));
    assert!(X86_64_LINKER.contains("${ld_data}"));
    assert!(DYNAMIC_LINKER.contains("__PERCPU_TSS_OFFSET = __PERCPU_TSS - _percpu_load_start"));
}
