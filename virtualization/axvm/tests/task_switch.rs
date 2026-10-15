//! The existing PMU/GPIO board workload, with a per-VM FIFO host policy.

#![cfg_attr(target_os = "none", no_std)]
#![no_main]

use ax_driver as _;
use ax_runtime as _;
use ax_std as _;

mod support;

#[axtest::tests]
mod tests {
    #[test]
    fn fifo_guest_task_switch_measurement() -> axtest::AxTestResult {
        super::support::case(super::run())
    }
}

fn run() -> anyhow::Result<()> {
    use std::time::Duration;

    use anyhow::{Context, ensure};

    let policy =
        axvm::SchedulePolicy::fifo(axvm::RtPriority::new(80).context("valid FIFO priority")?);
    support::with_vm("/guest/builtin/configs/vm-1.toml", policy, |fixture| {
        axvm::host::shutdown_filesystems().context("release host filesystem for passthrough")?;
        axvm::host::prepare_block_passthrough_device();
        fixture.vm.start()?.wait()?;
        fixture.wait(Duration::from_secs(30), |snapshot, _| {
            snapshot.cpu.running_vcpu_count == 1
        })?;
        fixture.check_policy(policy, 1)?;
        fixture.wait(Duration::from_secs(3500), |_, output| {
            output.lines().any(|line| line.trim() == "Bencher end")
        })?;
        fixture.validate_console(|output| {
            for index in 0..30 {
                ensure!(
                    output
                        .lines()
                        .filter(|line| line.starts_with(&format!(
                            "AXVISOR_TASK_SWITCH_GROUP_SUMMARY index={index} \
                             samples_per_direction=5000000 "
                        )))
                        .count()
                        == 1,
                    "missing or repeated switch round {index}"
                );
            }
            let summary = output
                .lines()
                .find(|line| line.starts_with("TASK_SWITCH_SUMMARY "))
                .context("guest omitted the aggregate measurement")?;
            for (field, expected) in [
                ("rounds", Some(30)),
                ("samples_per_round", Some(10_000_000)),
                ("avg_ns", None),
                ("avg_cycles", None),
            ] {
                let value = summary
                    .split_whitespace()
                    .find_map(|entry| {
                        let (key, value) = entry.split_once('=')?;
                        (key == field).then(|| value.parse::<u64>().ok()).flatten()
                    })
                    .with_context(|| format!("missing measurement field {field}"))?;
                ensure!(
                    expected.map_or(value > 0, |expected| value == expected),
                    "invalid measurement field {field}={value}"
                );
            }
            Ok(())
        })?;
        fixture.stop()
    })
}
