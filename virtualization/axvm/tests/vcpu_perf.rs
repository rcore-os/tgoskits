//! Board throughput benchmark under the existing CPU0 host contention workload.

#![cfg_attr(target_os = "none", no_std)]
#![no_main]

use ax_driver as _;
use ax_runtime as _;
use ax_std as _;

#[path = "support/load.rs"]
mod load;
mod support;

#[axtest::tests]
mod tests {
    #[test]
    fn guest_throughput_under_host_contention() -> axtest::AxTestResult {
        super::support::case(super::run())
    }
}

fn run() -> anyhow::Result<()> {
    use std::time::Duration;

    use anyhow::ensure;

    let policy = axvm::SchedulePolicy::default();
    support::with_vm("/guest/builtin/configs/vm-1.toml", policy, |fixture| {
        // Start before guest entry; finish only after every sample, result and
        // completion line has been validated. Error paths also join the load.
        let load = load::PerformanceLoad::start()?;
        fixture.vm.start()?.wait()?;
        fixture.wait(Duration::from_secs(90), |snapshot, output| {
            snapshot.cpu.running_vcpu_count == 1
                && output.lines().any(|line| line.trim() == "VCPU_PERF_PASS")
        })?;
        load.check_running()?;
        fixture.check_policy(policy, 1)?;
        fixture.flush_console()?;
        // The guest owns known-answer, wake-rate, timing and fixed-baseline
        // checks. Require its entire sampling/report protocol before cleanup.
        fixture.validate_console(|output| {
            for index in 0..=5 {
                ensure!(
                    output
                        .lines()
                        .filter(|line| line.starts_with(&format!("VCPU_PERF_SAMPLE index={index} ")))
                        .count()
                        == 1,
                    "missing or repeated sample {index}"
                );
            }
            ensure!(
                output.contains("VCPU_PERF_DONE windows=5"),
                "guest did not finish all windows"
            );
            ensure!(
                output.lines().any(|line| line.starts_with("VCPU_PERF_RESULT blocks_per_second=")),
                "guest omitted the benchmark result"
            );
            Ok(())
        })?;
        load.finish()?;
        fixture.stop()
    })
}
