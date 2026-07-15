//! Source-level contract for the runtime-owned execution-context identity.

const RUNTIME: &str = include_str!("../src/runtime.rs");
const TASK_SYSTEM: &str = include_str!("../src/system/task_system.rs");

#[test]
fn context_binding_is_a_value_only_runtime_abi() {
    assert!(RUNTIME.contains("#[repr(C)]\npub struct ContextThreadBinding"));
    assert!(RUNTIME.contains("pub context: ExecutionContextHandle"));
    assert!(RUNTIME.contains("#[repr(C)]\npub struct ThreadIdentityV1"));
    assert!(RUNTIME.contains("pub slot: u32"));
    assert!(RUNTIME.contains("pub generation: u32"));
    assert!(RUNTIME.contains("pub identity: ThreadIdentityV1"));
    assert!(!RUNTIME.contains("pub thread_id: u64"));
    assert!(RUNTIME.contains("fn bind_context_thread(binding: ContextThreadBinding)"));

    for forbidden in ["&Thread", "ThreadHandle", "Arc<", "dyn ", "*const Thread"] {
        let binding = RUNTIME
            .split_once("pub struct ContextThreadBinding")
            .expect("context-thread binding value must exist")
            .1
            .split_once('}')
            .expect("context-thread binding value must have a body")
            .0;
        assert!(
            !binding.contains(forbidden),
            "context-thread binding must not expose {forbidden} across trait-ffi"
        );
    }
}

#[test]
fn task_system_binds_an_allocated_identity_before_ready_publication() {
    let create = function_body(TASK_SYSTEM, "pub fn create_thread(");
    let allocate = create
        .find("let id = ThreadId::from_parts(slot, generation);")
        .expect("ThreadId allocation must remain explicit");
    let bind = create
        .find("task_runtime::bind_context_thread")
        .expect("a runtime context must be bound to its allocated ThreadId");
    let finish = create
        .find("Ok(ThreadHandle { core })")
        .expect("thread construction must have one success publication");

    assert!(
        allocate < bind && bind < finish,
        "context identity must be bound after ThreadId allocation and before callers can make it \
         Ready"
    );
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
