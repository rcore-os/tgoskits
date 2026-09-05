use std::os::arceos::{
    api::task::{AxCpuMask, ax_set_current_affinity},
    modules::ax_runtime::task::{
        CpuId, qperf_cpu_owner_claims, qperf_current_cpu_pin_entries, schedule_current_cpu,
    },
};

pub fn run() -> crate::TestResult {
    test_no_switch_scheduler_frame_avoids_redundant_owner_claim();
    Ok(())
}

fn test_no_switch_scheduler_frame_avoids_redundant_owner_claim() {
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());

    let cpu = CpuId::new(0);
    let mut minimum_owner_claims = u64::MAX;
    let mut minimum_pin_entries = u64::MAX;
    let mut no_switch_samples = 0;
    for _ in 0..64 {
        let owner_claims_before =
            qperf_cpu_owner_claims(cpu).expect("CPU 0 owner metrics must remain available");
        let pin_entries_before = qperf_current_cpu_pin_entries();
        let outcome = schedule_current_cpu().expect("scheduler safe point must remain available");
        let pin_entries_after = qperf_current_cpu_pin_entries();
        let owner_claims_after =
            qperf_cpu_owner_claims(cpu).expect("CPU 0 owner metrics must remain available");
        if outcome
            .decision()
            .is_some_and(|decision| decision.requires_context_switch())
        {
            continue;
        }
        minimum_owner_claims = minimum_owner_claims.min(owner_claims_after - owner_claims_before);
        minimum_pin_entries = minimum_pin_entries.min(pin_entries_after - pin_entries_before);
        no_switch_samples += 1;
        if no_switch_samples == 8 {
            break;
        }
    }
    assert_eq!(
        no_switch_samples, 8,
        "the bounded probe must observe eight no-switch scheduler frames"
    );
    assert_eq!(
        minimum_owner_claims, 0,
        "a quiescent no-switch scheduler frame must not claim CPU owner state when no owner work \
         was published"
    );
    assert_eq!(
        minimum_pin_entries, 2,
        "a quiescent no-switch scheduler frame must pin once for entry and once for exit"
    );
}
