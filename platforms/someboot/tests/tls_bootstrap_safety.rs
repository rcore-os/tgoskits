//! Source contracts for TLS setup that runs before ordinary task execution.

const AXHAL_TLS: &str = include_str!("../../../os/arceos/modules/axhal/src/tls.rs");
const LOONGARCH_BOOT: &str = include_str!("../src/arch/loongarch64/mod.rs");

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let source = &source[start..];
    let open = source.find('{').expect("function body must start");
    let mut depth = 0;
    for (offset, character) in source[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function `{signature}`")
}

#[test]
fn task_tls_checks_oom_before_pointer_arithmetic() {
    let allocate = function_body(AXHAL_TLS, "pub fn alloc() -> Self");
    let checked = allocate
        .find("handle_alloc_error(layout)")
        .expect("TLS allocation must use the allocator's fatal OOM path");
    let arithmetic = allocate
        .find("area_base.as_ptr().add(static_tls_offset())")
        .expect("TLS initialization must copy into the checked allocation");
    assert!(checked < arithmetic);
}

#[test]
fn loongarch_boot_tls_rejects_invalid_order_and_oversize() {
    let initialize = function_body(LOONGARCH_BOOT, "fn init_boot_tls()");
    assert!(initialize.contains("etbss < etdata"));
    assert!(initialize.contains("BOOT_TLS_SIZE"));
    assert!(initialize.contains("boot_tls_layout_fatal()"));
    assert!(
        !initialize.contains("if etdata < stdata || etbss < stdata {\n                return;")
    );
}
