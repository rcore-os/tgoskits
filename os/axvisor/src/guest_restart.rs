//! Build-configured, one-shot guest reset for restart-recovery evidence.

use std::{
    io::{self, Write},
    string::ToString,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use axvm::VmStatus;

const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const EVIDENCE_RECORD_COPIES: usize = 2;
const EVIDENCE_RECORD_PAUSE: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestRestartConfig {
    vm_id: usize,
    delay_ms: u64,
    ready_timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestRestartReport {
    vm_id: usize,
    ready_wait_ms: u64,
    requested_delay_ms: u64,
    observed_delay_ms: u64,
    before_status: VmStatus,
    after_status: VmStatus,
}

/// A build-configured worker that resets exactly one running guest once.
pub(crate) struct GuestRestartTask {
    worker: JoinHandle<Result<GuestRestartReport>>,
}

impl GuestRestartTask {
    /// Starts the one-shot reset worker when all three build settings exist.
    pub(crate) fn start_configured() -> Result<Option<Self>> {
        let Some(config) = configured_guest_restart()? else {
            return Ok(None);
        };
        validate_config(config)?;
        crate::manager::AxvmManager::with_vm(config.vm_id, |_| ())
            .with_context(|| format!("configured restart VM[{}] does not exist", config.vm_id))?;

        write_repeated_record(&format!(
            "AXVISOR_GUEST_RESTART_ARMED schema=1 vm_id={} delay_ms={} ready_timeout_ms={}",
            config.vm_id, config.delay_ms, config.ready_timeout_ms
        ))
        .context("write Axvisor guest-restart armed evidence")?;
        let worker = thread::Builder::new()
            .name("axvisor-guest-restart".to_string())
            .spawn(move || run_worker(config))
            .context("spawn Axvisor guest-restart task")?;
        Ok(Some(Self { worker }))
    }

    /// Joins the worker, validates the single reset, and publishes its report.
    pub(crate) fn join_and_publish(self) -> Result<()> {
        let report = self
            .worker
            .join()
            .map_err(|_| anyhow::anyhow!("Axvisor guest-restart task panicked"))??;
        validate_report(&report)?;
        write_repeated_record(&format_report(&report))
            .context("write Axvisor guest-restart completion evidence")?;
        write_repeated_record(&format_timing_report(&report))
            .context("write Axvisor guest-restart timing evidence")
    }
}

fn configured_guest_restart() -> Result<Option<GuestRestartConfig>> {
    parse_config(
        option_env!("AXVISOR_GUEST_RESTART_VM_ID"),
        option_env!("AXVISOR_GUEST_RESTART_DELAY_MS"),
        option_env!("AXVISOR_GUEST_RESTART_READY_TIMEOUT_MS"),
    )
}

fn parse_config(
    vm_id: Option<&str>,
    delay_ms: Option<&str>,
    ready_timeout_ms: Option<&str>,
) -> Result<Option<GuestRestartConfig>> {
    match (vm_id, delay_ms, ready_timeout_ms) {
        (None, None, None) => Ok(None),
        (Some(vm_id), Some(delay_ms), Some(ready_timeout_ms)) => Ok(Some(GuestRestartConfig {
            vm_id: vm_id
                .parse()
                .with_context(|| format!("parse AXVISOR_GUEST_RESTART_VM_ID `{vm_id}`"))?,
            delay_ms: delay_ms
                .parse()
                .with_context(|| format!("parse AXVISOR_GUEST_RESTART_DELAY_MS `{delay_ms}`"))?,
            ready_timeout_ms: ready_timeout_ms.parse().with_context(|| {
                format!("parse AXVISOR_GUEST_RESTART_READY_TIMEOUT_MS `{ready_timeout_ms}`")
            })?,
        })),
        _ => bail!(
            "AXVISOR_GUEST_RESTART_VM_ID, AXVISOR_GUEST_RESTART_DELAY_MS, and \
             AXVISOR_GUEST_RESTART_READY_TIMEOUT_MS must be configured together"
        ),
    }
}

fn validate_config(config: GuestRestartConfig) -> Result<()> {
    if config.delay_ms == 0 {
        bail!("guest-restart delay must be positive");
    }
    if config.ready_timeout_ms == 0 {
        bail!("guest-restart ready timeout must be positive");
    }
    Ok(())
}

fn run_worker(config: GuestRestartConfig) -> Result<GuestRestartReport> {
    let ready_started = Instant::now();
    let ready_timeout = Duration::from_millis(config.ready_timeout_ms);
    loop {
        let status = crate::manager::AxvmManager::with_vm(config.vm_id, |vm| vm.status())
            .with_context(|| format!("restart target VM[{}] disappeared", config.vm_id))?;
        if status == VmStatus::Running {
            break;
        }
        if status.is_terminal() {
            bail!(
                "restart target VM[{}] became {status} before running",
                config.vm_id
            );
        }
        if ready_started.elapsed() >= ready_timeout {
            bail!(
                "restart target VM[{}] did not run within {} ms; last status={status}",
                config.vm_id,
                config.ready_timeout_ms
            );
        }
        wait_cooperatively(STATUS_POLL_INTERVAL);
    }
    let ready_wait_ms = elapsed_millis(ready_started);
    write_repeated_record(&format!(
        "AXVISOR_GUEST_RESTART_RUNNING schema=1 vm_id={} ready_wait_ms={} status=running",
        config.vm_id, ready_wait_ms
    ))
    .context("write Axvisor guest-restart running evidence")?;

    let delay_started = Instant::now();
    wait_cooperatively(Duration::from_millis(config.delay_ms));
    let observed_delay_ms = elapsed_millis(delay_started);
    let before_status = crate::manager::AxvmManager::with_vm(config.vm_id, |vm| vm.status())
        .with_context(|| format!("restart target VM[{}] disappeared", config.vm_id))?;
    if before_status != VmStatus::Running {
        bail!(
            "restart target VM[{}] is {before_status} at trigger time",
            config.vm_id
        );
    }
    write_repeated_record(&format!(
        "AXVISOR_GUEST_RESTART_TRIGGER schema=1 vm_id={} requested_delay_ms={} \
         observed_delay_ms={} before_status=running reset_count=1",
        config.vm_id, config.delay_ms, observed_delay_ms
    ))
    .context("write Axvisor guest-restart trigger evidence")?;

    crate::manager::AxvmManager::reset_vm(config.vm_id)?;
    let after_status = crate::manager::AxvmManager::with_vm(config.vm_id, |vm| vm.status())
        .with_context(|| format!("reset VM[{}] disappeared", config.vm_id))?;
    Ok(GuestRestartReport {
        vm_id: config.vm_id,
        ready_wait_ms,
        requested_delay_ms: config.delay_ms,
        observed_delay_ms,
        before_status,
        after_status,
    })
}

fn elapsed_millis(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn wait_cooperatively(duration: Duration) {
    let started = Instant::now();
    while started.elapsed() < duration {
        // The RK3588 host sleep timer can stop waking this task after guest
        // virtual timers start. Keeping it runnable preserves guest scheduling.
        thread::yield_now();
    }
}

fn validate_report(report: &GuestRestartReport) -> Result<()> {
    if report.observed_delay_ms < report.requested_delay_ms {
        bail!(
            "guest-restart delay was {} ms, below requested {} ms",
            report.observed_delay_ms,
            report.requested_delay_ms
        );
    }
    if report.before_status != VmStatus::Running || report.after_status != VmStatus::Running {
        bail!(
            "guest-restart VM[{}] transitioned from {} to {} instead of running to running",
            report.vm_id,
            report.before_status,
            report.after_status
        );
    }
    Ok(())
}

fn format_report(report: &GuestRestartReport) -> String {
    format!(
        "AXVISOR_GUEST_RESTART_COMPLETE schema=1 vm_id={} before_status={} after_status={} \
         reset_count=1",
        report.vm_id, report.before_status, report.after_status
    )
}

fn format_timing_report(report: &GuestRestartReport) -> String {
    format!(
        "AXVISOR_GUEST_RESTART_TIMING schema=1 vm_id={} ready_wait_ms={} \
         requested_delay_ms={} observed_delay_ms={}",
        report.vm_id, report.ready_wait_ms, report.requested_delay_ms, report.observed_delay_ms
    )
}

fn write_repeated_record(record: &str) -> io::Result<()> {
    let mut output = std::io::stdout();
    for copy in 0..EVIDENCE_RECORD_COPIES {
        writeln!(output, "{record}")?;
        output.flush()?;
        if copy + 1 < EVIDENCE_RECORD_COPIES {
            wait_cooperatively(EVIDENCE_RECORD_PAUSE);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_configuration_requires_a_complete_positive_triple() {
        assert_eq!(parse_config(None, None, None).unwrap(), None);
        assert!(parse_config(Some("1"), Some("12000"), None).is_err());
        assert_eq!(
            parse_config(Some("1"), Some("12000"), Some("30000")).unwrap(),
            Some(GuestRestartConfig {
                vm_id: 1,
                delay_ms: 12_000,
                ready_timeout_ms: 30_000,
            })
        );
        assert!(
            validate_config(GuestRestartConfig {
                vm_id: 1,
                delay_ms: 0,
                ready_timeout_ms: 30_000,
            })
            .is_err()
        );
    }

    #[test]
    fn completion_record_proves_one_running_to_running_reset() {
        let report = GuestRestartReport {
            vm_id: 1,
            ready_wait_ms: 425,
            requested_delay_ms: 12_000,
            observed_delay_ms: 12_001,
            before_status: VmStatus::Running,
            after_status: VmStatus::Running,
        };

        validate_report(&report).unwrap();
        assert_eq!(
            format_report(&report),
            "AXVISOR_GUEST_RESTART_COMPLETE schema=1 vm_id=1 before_status=running \
             after_status=running reset_count=1"
        );
        assert_eq!(
            format_timing_report(&report),
            "AXVISOR_GUEST_RESTART_TIMING schema=1 vm_id=1 ready_wait_ms=425 \
             requested_delay_ms=12000 observed_delay_ms=12001"
        );
    }
}
