//! Host CPU-local identity must be restored before AxVM resumes host work.

const VCPU: &str = include_str!("../src/vcpu.rs");

#[test]
fn every_backend_transition_verifies_the_live_host_binding() {
    assert!(VCPU.contains("fn assert_host_cpu_binding(&self"));
    assert!(VCPU.contains("ax_percpu::bound_current(self.cpu_pin)"));

    for (operation, marker) in [
        ("pub(crate) fn bind(", "after vCPU bind"),
        ("pub(crate) fn run<'cpu>(", "after guest exit"),
        ("pub(crate) fn unbind(", "after vCPU unbind"),
    ] {
        let body = function_body(VCPU, operation);
        let backend = body
            .find("unsafe { self.arch_vcpu_mut_reserved() }")
            .unwrap_or_else(|| panic!("{operation} must call its reserved backend"));
        let verify = body
            .find(&format!("assert_host_cpu_binding(\"{marker}\")"))
            .unwrap_or_else(|| panic!("{operation} must verify host restoration"));
        let finish = body
            .find("self.finish_reserved_state(")
            .unwrap_or_else(|| panic!("{operation} must finish its lifecycle reservation"));
        let translate = body
            .find("map_vcpu_backend_error")
            .unwrap_or_else(|| panic!("{operation} must translate backend failures"));

        assert!(
            backend < verify && verify < translate && translate < finish,
            "host CPU identity must be verified before error conversion and lifecycle publication"
        );
    }
}

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    source
        .split_once(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"))
        .1
        .split_once("\n    ///")
        .map_or_else(
            || panic!("unterminated function `{signature}`"),
            |(body, _)| body,
        )
}
