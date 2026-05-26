#[test]
fn public_api_matches_tool_centered_runtime_construction() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass_tool_configs.rs");
    t.compile_fail("tests/ui/fail_cargo_builder.rs");
    t.compile_fail("tests/ui/fail_qemu_override.rs");
    t.compile_fail("tests/ui/fail_runner_path_field.rs");
}
