use std::fs;

fn source(relative_path: &str) -> String {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(manifest_dir.join(relative_path))
        .unwrap_or_else(|error| panic!("{relative_path} must be readable: {error}"))
}

#[test]
fn loongarch_timer_capability_never_owns_the_hard_irq_or_hardware_timer() {
    let capability = source("src/arch/loongarch64/capabilities.rs");
    let backend = source("src/arch/loongarch64/mod.rs");

    for forbidden in [
        "register_timer_callback",
        "ax_task::register_timer_callback",
        "ax_hal::time::set_oneshot_timer",
        "crate::check_timer_events()",
    ] {
        assert!(
            !capability.contains(forbidden),
            "LoongArch capability must not run {forbidden} from a hard timer IRQ"
        );
    }

    assert!(
        backend.contains("crate::timer::register_timer(deadline.as_nanos() as u64, callback)"),
        "guest timer callbacks must enter the AxVM task-context timer wheel"
    );
    assert!(
        backend.contains("crate::timer::cancel_timer(token)"),
        "guest timer cancellation must use the same timer-wheel owner"
    );
}

#[test]
fn loongarch_timer_capacity_failure_is_visible_to_the_guest_csr_model() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("axvm must live two levels below the workspace root");
    let host_ops = fs::read_to_string(workspace.join("virtualization/loongarch_vcpu/src/host.rs"))
        .expect("LoongArch host operations must be readable");
    let guest_csr =
        fs::read_to_string(workspace.join("virtualization/loongarch_vcpu/src/guest_csr.rs"))
            .expect("LoongArch guest CSR model must be readable");
    let backend = source("src/arch/loongarch64/mod.rs");

    assert!(
        host_ops.contains(") -> Option<usize>;"),
        "the OS-neutral timer capability must report capacity exhaustion"
    );
    assert!(
        backend.contains(") -> Option<usize> {")
            && backend
                .contains("crate::timer::register_timer(deadline.as_nanos() as u64, callback)"),
        "AxVM must propagate its fallible timer-wheel registration"
    );
    assert!(
        guest_csr.contains("let Some(token) = H::register_timer(")
            && guest_csr.contains("mark_guest_timer_expired(ctx);"),
        "capacity exhaustion must become an immediate guest timer event instead of a lost timer"
    );
}

#[test]
fn loongarch_idle_uses_scheduler_sleep_without_owning_local_irq_state() {
    let idle = source("src/arch/loongarch64/idle.rs");

    for forbidden in [
        "crate::check_timer_events()",
        "set_timer_irq_enabled",
        "enable_irqs",
        "disable_irqs",
        "busy_wait",
    ] {
        assert!(
            !idle.contains(forbidden),
            "LoongArch idle must not call `{forbidden}`"
        );
    }

    let pending_check = idle
        .find("if has_pending_interrupt")
        .expect("idle must retain the enabled-pending-interrupt fast path");
    let scheduler_sleep = idle
        .find("ax_std::thread::sleep(idle_timeout);")
        .expect("idle must sleep through the scheduler");
    assert!(
        pending_check < scheduler_sleep,
        "the pending interrupt fast path must precede scheduler sleep"
    );
}
