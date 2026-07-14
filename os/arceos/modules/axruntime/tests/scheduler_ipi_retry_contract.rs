use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("axruntime must live below the workspace os directory")
        .to_path_buf()
}

fn source(relative: &str) -> String {
    fs::read_to_string(workspace_root().join(relative))
        .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"))
}

#[test]
fn runtime_maps_retry_and_invalid_ipi_results() {
    let runtime = source("os/arceos/modules/axruntime/src/task.rs");
    assert!(runtime.contains("IpiSendStatus::Success => RuntimeStatus::Success"));
    assert!(runtime.contains("IpiSendStatus::Retry => RuntimeStatus::Busy"));
    assert!(runtime.contains("IpiSendStatus::Invalid => RuntimeStatus::InvalidArgument"));
}

#[test]
fn scheduler_send_failure_releases_the_coalescing_latch() {
    let cpu = source("components/ax-task/src/system/cpu.rs");
    assert!(cpu.contains("finish_scheduler_ipi_send"));
    assert!(cpu.contains("send_claimed_scheduler_ipi"));
    assert!(cpu.contains("SchedulerIpiClaim"));
    assert!(cpu.contains("scheduler_ipi_fault_count"));
    assert!(cpu.contains("SchedulerIpiRetrySet"));
    assert!(cpu.contains("Ordering::Release"));

    for relative in [
        "components/ax-task/src/thread/handle.rs",
        "components/ax-task/src/system/task_system.rs",
    ] {
        let caller = source(relative);
        assert!(
            !caller.contains("task_runtime::send_scheduler_ipi"),
            "{relative} must not bypass typed latch completion"
        );
    }
}

#[test]
fn generic_callback_ipi_does_not_acknowledge_a_scheduler_epoch() {
    let runtime = source("os/arceos/modules/axruntime/src/task.rs");
    let body = runtime
        .split_once("pub(crate) fn on_scheduler_ipi()")
        .unwrap()
        .1
        .split_once("fn initialize_current_cpu")
        .unwrap()
        .0;
    assert!(!body.contains("acknowledge_scheduler_ipi"));
    assert!(body.contains("needs_reschedule"));
}

#[test]
fn stuck_callback_ipi_retry_cannot_starve_local_runnable_work() {
    let runtime = source("os/arceos/modules/axruntime/src/task.rs");
    let idle = runtime
        .split_once("pub(crate) fn run_idle() -> !")
        .expect("idle loop must exist")
        .1
        .split_once("enum IdleEntryAction")
        .expect("idle loop must remain focused")
        .0;
    let service = idle
        .find("service_callback_ipi_retries(64)")
        .expect("idle must make bounded callback IPI retry progress");
    let schedule = idle
        .find("ax_task::schedule_current_cpu()")
        .expect("idle must enter the local scheduler every iteration");
    assert!(service < schedule);
    assert!(
        !idle[service..schedule].contains("continue;"),
        "transport Busy must not skip runnable local scheduler work",
    );
}

#[test]
fn callback_ipi_retry_is_a_final_wfi_gate() {
    let runtime = source("os/arceos/modules/axruntime/src/task.rs");
    let wait = runtime
        .split_once("fn wait_for_interrupt()")
        .expect("runtime WFI hook must exist")
        .1
        .split_once("fn allocate_stack")
        .expect("runtime WFI hook must remain focused")
        .0;
    let retry = wait
        .find("callback_ipi_retry_pending()")
        .expect("WFI gate must observe persistent callback IPI retry state");
    let early_return = wait[retry..]
        .find("return;")
        .expect("pending retry must reject WFI");
    let wfi = wait
        .find("ax_hal::asm::wait_for_irqs()")
        .expect("runtime must eventually enter WFI");
    assert!(retry + early_return < wfi);
}
