use std::os::arceos::{
    api::task::{AxCpuMask, ax_set_current_affinity},
    modules::ax_runtime::task::{CpuId, qperf_cpu_owner_claims, schedule_current_cpu},
};

pub fn run() -> crate::TestResult {
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());

    let cpu = CpuId::new(0);
    let before = qperf_cpu_owner_claims(cpu).expect("CPU 0 owner metrics must be available");
    let outcome = schedule_current_cpu().expect("scheduler safe point must be available");
    let after = qperf_cpu_owner_claims(cpu).expect("CPU 0 owner metrics must remain available");

    assert!(
        outcome
            .decision()
            .is_none_or(|decision| !decision.requires_context_switch()),
        "the isolated single-CPU probe must not switch execution contexts"
    );
    assert_eq!(
        after - before,
        1,
        "one no-switch scheduler frame must retain one CPU owner claim through its final request \
         observation"
    );
    Ok(())
}
