use std::os::arceos::{
    api::task::{AxCpuMask, ax_set_current_affinity},
    modules::ax_runtime::task::{
        CpuId, qperf_cpu_owner_claims, qperf_current_cpu_pin_entries, schedule_current_cpu,
    },
};

pub fn run() -> crate::TestResult {
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());

    let cpu = CpuId::new(0);
    let before = qperf_cpu_owner_claims(cpu).expect("CPU 0 owner metrics must be available");
    let pin_entries_before = qperf_current_cpu_pin_entries();
    let outcome = schedule_current_cpu().expect("scheduler safe point must be available");
    let pin_entries_after = qperf_current_cpu_pin_entries();
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
    let mut minimum_pin_entries = pin_entries_after - pin_entries_before;
    for _ in 0..7 {
        let before = qperf_current_cpu_pin_entries();
        let outcome = schedule_current_cpu().expect("scheduler safe point must remain available");
        let after = qperf_current_cpu_pin_entries();
        assert!(
            outcome
                .decision()
                .is_none_or(|decision| !decision.requires_context_switch()),
            "the repeated single-CPU probe must not switch execution contexts"
        );
        minimum_pin_entries = minimum_pin_entries.min(after - before);
    }
    assert_eq!(
        minimum_pin_entries, 7,
        "one no-switch scheduler call must reuse its exit CPU pin for deferred clockevent rearm"
    );
    Ok(())
}
