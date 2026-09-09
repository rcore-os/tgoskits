#[test]
fn invalid_interfaces_and_unsafe_calls_have_compiler_diagnostics() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
