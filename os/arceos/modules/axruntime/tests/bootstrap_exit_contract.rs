//! Primary-bootstrap exit is system termination, not joinable-thread exit.

const TASK_RUNTIME: &str = include_str!("../src/task.rs");

#[test]
fn primary_bootstrap_exit_precedes_runtime_completion_publication() {
    assert!(
        TASK_RUNTIME.contains("static PRIMARY_BOOTSTRAP_THREAD:"),
        "the runtime must retain the primary bootstrap's typed identity"
    );

    let exit = function_body(TASK_RUNTIME, "pub fn exit_current(");
    let bootstrap = exit
        .find("PRIMARY_BOOTSTRAP_THREAD")
        .expect("exit_current must classify the primary bootstrap");
    let prepare = exit
        .find("prepare_current_exit")
        .expect("ordinary runtime threads must prepare scheduler exit");
    let publish = exit
        .find("publish_current_runtime_exit")
        .expect("ordinary runtime threads must publish join completion");

    assert!(
        bootstrap < prepare && prepare < publish,
        "bootstrap shutdown must happen before the joinable-thread exit transaction"
    );
    assert!(
        exit.contains("ax_hal::power::system_off()"),
        "the primary bootstrap owns whole-system termination"
    );
}

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    source
        .split_once(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"))
        .1
        .split_once("\n}\n")
        .map_or_else(
            || panic!("unterminated function `{signature}`"),
            |(body, _)| body,
        )
}
