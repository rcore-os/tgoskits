use std::fs;

#[test]
fn exec_and_scheduler_share_the_runtime_owned_address_space_installer() {
    let source = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/task.rs"))
        .expect("ax-runtime task source must be readable");

    let exec = function_source(
        &source,
        "pub fn switch_current_page_table",
        "#[cfg(not(feature = \"fs\"))]",
    );
    assert!(
        exec.contains("install_runtime_address_space(address_space)"),
        "exec must install its replacement through the runtime-owned operation"
    );
    for forbidden in ["write_user_page_table", "flush_tlb"] {
        assert!(
            !exec.contains(forbidden),
            "exec must not bypass TaskRuntime ownership with {forbidden}"
        );
    }

    let runtime_impl = function_source(
        &source,
        "fn install_address_space(address_space: AddressSpaceHandle)",
        "fn flush_tlb_local",
    );
    assert!(
        runtime_impl.contains("install_runtime_address_space(address_space)"),
        "TaskRuntime must delegate to the same runtime-owned installer as exec"
    );
    for forbidden in ["write_user_page_table", "flush_tlb"] {
        assert!(
            !runtime_impl.contains(forbidden),
            "the trait-FFI adapter must not duplicate architecture operation {forbidden}"
        );
    }

    let installer = function_source(
        &source,
        "fn install_runtime_address_space(address_space: AddressSpaceHandle)",
        "fn destroy_runtime_context",
    );
    assert!(installer.contains("write_user_page_table"));
    assert!(installer.contains("flush_tlb"));
}

fn function_source<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let body = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing function start {start:?}"))
        .1;
    body.split_once(end)
        .unwrap_or_else(|| panic!("missing function end {end:?}"))
        .0
}
