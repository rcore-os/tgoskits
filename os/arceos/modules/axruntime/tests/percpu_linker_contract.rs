//! Source-level contract for the fixed per-CPU area prefix.

const SOMEBOOT_DATA: &str = include_str!("../../../../../platforms/someboot/src/ld/data.ld");
const AXPLAT_LINKER: &str = include_str!("../../axhal/axplat.lds.S");
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
fn every_runtime_linker_layout_places_the_cpu_area_prefix_at_offset_zero() {
    assert_prefix_is_first(SOMEBOOT_DATA, "someboot data template");
    assert_prefix_is_first(AXPLAT_LINKER, "axplat linker script");
    assert_prefix_is_first(SCOPE_LOCAL_LINKER, "scope-local linker script");
}

#[test]
fn x86_trap_uses_a_template_offset_with_an_area_base_gs_anchor() {
    assert!(X86_TRAP.contains("offset __PERCPU_TSS_OFFSET"));
    assert!(
        !X86_TRAP.contains("offset __PERCPU_TSS +"),
        "a runtime-area GS base must not add the absolute template VMA",
    );
    assert!(SOMEBOOT_DATA.contains("__PERCPU_TSS_OFFSET = __PERCPU_TSS - __percpu_start"));
    assert!(AXPLAT_LINKER.contains("__PERCPU_TSS_OFFSET = __PERCPU_TSS - _percpu_load_start"));
    assert!(DYNAMIC_LINKER.contains("__PERCPU_TSS_OFFSET = __PERCPU_TSS - _percpu_load_start"));
}
