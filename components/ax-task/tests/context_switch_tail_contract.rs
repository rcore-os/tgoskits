//! Context resources must leave their CPU before scheduler reclamation starts.

const RUNTIME: &str = include_str!("../src/runtime.rs");
const TASK_SYSTEM: &str = include_str!("../src/system/task_system.rs");

#[test]
fn runtime_tail_precedes_scheduler_on_cpu_release() {
    assert!(
        RUNTIME.contains("fn finish_context_switch_tail() -> RuntimeStatus"),
        "TaskRuntime must expose an allocation-free context-resource tail"
    );

    let tail = function_body(TASK_SYSTEM, "pub fn complete_context_switch(");
    let runtime = tail
        .find("task_runtime::finish_context_switch_tail()")
        .expect("context tail must withdraw runtime CPU ownership");
    let scheduler = tail
        .find("let mut state = self.state.lock()")
        .expect("context tail must release scheduler on_cpu ownership");
    let take = tail
        .find("take_switch_handoff()")
        .expect("context tail must consume its handoff only after runtime success");

    assert!(
        runtime < take && take < scheduler,
        "runtime context ownership must be withdrawn before on_cpu permits reclamation"
    );
}

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    source
        .split_once(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"))
        .1
        .split_once("\n}")
        .map_or_else(
            || panic!("unterminated function `{signature}`"),
            |(body, _)| body,
        )
}
