//! Source-level ownership contracts for current-CPU placement leases.

const LEASE: &str = include_str!("../src/system/current_cpu_lease.rs");
const FACADE: &str = include_str!("../src/facade.rs");
const THREAD_SCHED: &str = include_str!("../src/system/thread_sched.rs");
const TASK_SYSTEM: &str = include_str!("../src/system/task_system.rs");

#[test]
fn lease_is_non_send_and_generation_checked() {
    assert!(LEASE.contains("PhantomData<*mut ()>"));
    assert!(LEASE.contains("release_current_cpu_pin(self.cpu, self.generation)"));
    assert!(THREAD_SCHED.contains("struct CurrentCpuPinState"));
    for field in ["cpu: Option<CpuId>", "generation: u64", "count: usize"] {
        assert!(
            THREAD_SCHED.contains(field),
            "placement pin state must retain `{field}`"
        );
    }
}

#[test]
fn object_api_requires_exclusive_cpu_owner_access() {
    let pin = function_signature(TASK_SYSTEM, "pub fn pin_current_cpu(");
    assert!(
        pin.contains("cpu: Pin<&mut CpuLocal>"),
        "the object API must not read owner-only CpuLocal state through a shared reference"
    );
    assert!(
        !pin.contains("cpu: Pin<&CpuLocal>"),
        "a shared CpuLocal reference is not an owner capability"
    );

    let facade = function_body(FACADE, "pub fn pin_current_cpu()");
    assert!(
        facade.contains("as_pin_mut()"),
        "the safe facade must derive exclusive access from its IRQ-pinned owner claim"
    );
}

#[test]
fn every_production_migration_uses_the_pin_aware_authority() {
    let production = TASK_SYSTEM
        .split_once("#[cfg(test)]\nmod tests")
        .expect("task-system tests remain a final source appendix")
        .0;
    assert!(
        !production.contains("migration_target = Some"),
        "production migration must use ThreadSchedState::request_migration"
    );
    assert!(
        function_body(production, "pub fn set_affinity(")
            .contains("ensure_affinity_change_allowed")
    );
    assert!(
        function_body(production, "pub fn set_current_affinity(")
            .contains("ensure_affinity_change_allowed")
    );
    assert!(
        function_body(production, "fn select_owner_balance_candidate(")
            .contains("is_migration_pinned")
    );
    assert!(
        function_body(production, "fn enqueue_owner_thread_locked(")
            .contains("ensure_placement_cpu")
    );
    assert!(
        function_body(production, "fn schedule_out_owner_running(").contains("request_migration")
    );
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .expect("function signature must exist");
    let body_start = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("function body must start");
    let mut depth = 0_usize;
    for (offset, byte) in source.as_bytes()[body_start..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..=body_start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("function body must end")
}

fn function_signature<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .expect("function signature must exist");
    let end = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("function body must start");
    &source[start..end]
}
