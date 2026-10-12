use ax_std::os::arceos::modules::{
    ax_runtime::test_support::{disable_scheduler_ipi_for_axtest, enable_scheduler_ipi_for_axtest},
    ax_task::runtime::cpu::rearm_scheduler_work_for_axtest,
};

pub fn run() -> crate::TestResult {
    disable_scheduler_ipi_for_axtest();
    rearm_scheduler_work_for_axtest().map_err(|_| "owner scheduler rearm failed")?;
    enable_scheduler_ipi_for_axtest();
    println!("task_scheduler_local_rearm: local owner work survived unavailable IPI");
    Ok(())
}
