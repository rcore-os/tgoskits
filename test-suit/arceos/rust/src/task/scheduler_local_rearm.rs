use ax_std::os::arceos::modules::{
    ax_runtime::thread::creation_probe::{
        disable_scheduler_ipi_for_test, enable_scheduler_ipi_for_test,
    },
    ax_task::runtime::cpu::rearm_scheduler_work_for_fault_injection,
};

pub fn run() -> crate::TestResult {
    disable_scheduler_ipi_for_test();
    let result = rearm_scheduler_work_for_fault_injection();
    enable_scheduler_ipi_for_test();
    result.map_err(|_| "owner scheduler rearm failed")?;
    println!("task_scheduler_local_rearm: owner work survived unavailable IPI");
    Ok(())
}
