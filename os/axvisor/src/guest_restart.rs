//! Build-configured, one-shot guest reset for restart-recovery evidence.

use core::sync::atomic::{AtomicU8, Ordering};
use std::{
    io::{self, Write},
    string::ToString,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use ax_std::os::arceos::{api, modules};
use axvm::VmStatus;

const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const EVIDENCE_RECORD_COPIES: usize = 2;
const EVIDENCE_RECORD_PAUSE: Duration = Duration::from_millis(10);
const WORKER_STARTING: u8 = 0;
const WORKER_READY: u8 = 1;
const WORKER_FAILED: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestRestartConfig {
    vm_id: usize,
    host_cpu: usize,
    delay_ms: u64,
    ready_timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestRestartReport {
    vm_id: usize,
    host_cpu: usize,
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
        validate_config(config, api::sys::ax_get_cpu_num())?;
        let snapshot_bytes =
            crate::manager::AxvmManager::with_vm(config.vm_id, |vm| vm.capture_reset_memory())
                .with_context(|| format!("configured restart VM[{}] does not exist", config.vm_id))?
                .with_context(|| format!("capture pristine memory for VM[{}]", config.vm_id))?;
        info!(
            "Captured {snapshot_bytes} bytes of pristine reset memory for VM[{}]",
            config.vm_id
        );

        write_repeated_record(&format!(
            "AXVISOR_GUEST_RESTART_ARMED schema=1 vm_id={} host_cpu={} delay_ms={} \
             ready_timeout_ms={}",
            config.vm_id, config.host_cpu, config.delay_ms, config.ready_timeout_ms
        ))
        .context("write Axvisor guest-restart armed evidence")?;
        let worker_state = Arc::new(AtomicU8::new(WORKER_STARTING));
        let observed_worker_state = worker_state.clone();
        let worker = thread::Builder::new()
            .name("axvisor-guest-restart".to_string())
            .spawn(move || run_worker(config, worker_state))
            .context("spawn Axvisor guest-restart task")?;

        while observed_worker_state.load(Ordering::Acquire) == WORKER_STARTING {
            thread::yield_now();
        }
        if observed_worker_state.load(Ordering::Acquire) == WORKER_FAILED {
            return match join_worker(worker) {
                Err(error) => Err(error)
                    .context("Axvisor guest-restart task failed before publishing readiness"),
                Ok(_) => bail!(
                    "Axvisor guest-restart worker reported failure without returning an error"
                ),
            };
        }
        Ok(Some(Self { worker }))
    }

    /// Joins the worker, validates the single reset, and publishes its report.
    pub(crate) fn join_and_publish(self) -> Result<()> {
        let report = join_worker(self.worker)?;
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
        option_env!("AXVISOR_GUEST_RESTART_CPU"),
        option_env!("AXVISOR_GUEST_RESTART_DELAY_MS"),
        option_env!("AXVISOR_GUEST_RESTART_READY_TIMEOUT_MS"),
    )
}

fn parse_config(
    vm_id: Option<&str>,
    host_cpu: Option<&str>,
    delay_ms: Option<&str>,
    ready_timeout_ms: Option<&str>,
) -> Result<Option<GuestRestartConfig>> {
    match (vm_id, host_cpu, delay_ms, ready_timeout_ms) {
        (None, None, None, None) => Ok(None),
        (Some(vm_id), Some(host_cpu), Some(delay_ms), Some(ready_timeout_ms)) => {
            Ok(Some(GuestRestartConfig {
                vm_id: vm_id
                    .parse()
                    .with_context(|| format!("parse AXVISOR_GUEST_RESTART_VM_ID `{vm_id}`"))?,
                host_cpu: host_cpu
                    .parse()
                    .with_context(|| format!("parse AXVISOR_GUEST_RESTART_CPU `{host_cpu}`"))?,
                delay_ms: delay_ms.parse().with_context(|| {
                    format!("parse AXVISOR_GUEST_RESTART_DELAY_MS `{delay_ms}`")
                })?,
                ready_timeout_ms: ready_timeout_ms.parse().with_context(|| {
                    format!("parse AXVISOR_GUEST_RESTART_READY_TIMEOUT_MS `{ready_timeout_ms}`")
                })?,
            }))
        }
        _ => bail!(
            "AXVISOR_GUEST_RESTART_VM_ID, AXVISOR_GUEST_RESTART_CPU, \
             AXVISOR_GUEST_RESTART_DELAY_MS, and AXVISOR_GUEST_RESTART_READY_TIMEOUT_MS must be \
             configured together"
        ),
    }
}

fn validate_config(config: GuestRestartConfig, cpu_count: usize) -> Result<()> {
    if config.host_cpu >= cpu_count {
        bail!(
            "guest-restart host CPU {} is outside the initialized CPU count {}",
            config.host_cpu,
            cpu_count
        );
    }
    if config.host_cpu >= u128::BITS as usize {
        bail!(
            "guest-restart host CPU {} cannot be represented in evidence",
            config.host_cpu
        );
    }
    if config.delay_ms == 0 {
        bail!("guest-restart delay must be positive");
    }
    if config.ready_timeout_ms == 0 {
        bail!("guest-restart ready timeout must be positive");
    }
    Ok(())
}

fn run_worker(
    config: GuestRestartConfig,
    worker_state: Arc<AtomicU8>,
) -> Result<GuestRestartReport> {
    let result = prepare_and_run_worker(config, &worker_state);
    if result.is_err() {
        worker_state.store(WORKER_FAILED, Ordering::Release);
    }
    result
}

fn prepare_and_run_worker(
    config: GuestRestartConfig,
    worker_state: &AtomicU8,
) -> Result<GuestRestartReport> {
    let affinity = api::task::AxCpuMask::one_shot(config.host_cpu);
    api::task::ax_set_current_affinity(affinity)
        .map_err(|error| anyhow::anyhow!("apply singleton guest-restart CPU affinity: {error}"))?;
    let actual_cpu = modules::ax_hal::percpu::this_cpu_id();
    if actual_cpu != config.host_cpu {
        bail!(
            "guest-restart affinity requested pCPU{} but readiness ran on pCPU{}",
            config.host_cpu,
            actual_cpu
        );
    }
    write_repeated_record(&format!(
        "AXVISOR_GUEST_RESTART_PLACED schema=1 vm_id={} requested_pcpu={} actual_pcpu={} \
         affinity_mask={}",
        config.vm_id,
        config.host_cpu,
        actual_cpu,
        1_u128 << config.host_cpu
    ))
    .context("write Axvisor guest-restart placement evidence")?;
    worker_state.store(WORKER_READY, Ordering::Release);
    run_restart(config)
}

fn run_restart(config: GuestRestartConfig) -> Result<GuestRestartReport> {
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
        spin_for_duration(STATUS_POLL_INTERVAL);
    }
    let ready_wait_ms = elapsed_millis(ready_started);
    write_repeated_record(&format!(
        "AXVISOR_GUEST_RESTART_RUNNING schema=1 vm_id={} host_cpu={} ready_wait_ms={} \
         status=running",
        config.vm_id, config.host_cpu, ready_wait_ms
    ))
    .context("write Axvisor guest-restart running evidence")?;

    let delay_started = Instant::now();
    spin_for_duration(Duration::from_millis(config.delay_ms));
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
        "AXVISOR_GUEST_RESTART_TRIGGER schema=1 vm_id={} host_cpu={} requested_delay_ms={} \
         observed_delay_ms={} before_status=running reset_count=1",
        config.vm_id, config.host_cpu, config.delay_ms, observed_delay_ms
    ))
    .context("write Axvisor guest-restart trigger evidence")?;

    crate::manager::AxvmManager::reset_vm_with_spin_wait(config.vm_id)?;
    let after_status = crate::manager::AxvmManager::with_vm(config.vm_id, |vm| vm.status())
        .with_context(|| format!("reset VM[{}] disappeared", config.vm_id))?;
    Ok(GuestRestartReport {
        vm_id: config.vm_id,
        host_cpu: config.host_cpu,
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

fn spin_for_duration(duration: Duration) {
    let started = Instant::now();
    while started.elapsed() < duration {
        // This worker owns a reserved host CPU because running vCPU tasks can
        // otherwise starve a timer-blocked or voluntarily yielding host task.
        core::hint::spin_loop();
    }
}

fn join_worker(worker: JoinHandle<Result<GuestRestartReport>>) -> Result<GuestRestartReport> {
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("Axvisor guest-restart task panicked"))?
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
        "AXVISOR_GUEST_RESTART_COMPLETE schema=1 vm_id={} host_cpu={} before_status={} \
         after_status={} reset_count=1",
        report.vm_id, report.host_cpu, report.before_status, report.after_status
    )
}

fn format_timing_report(report: &GuestRestartReport) -> String {
    format!(
        "AXVISOR_GUEST_RESTART_TIMING schema=1 vm_id={} host_cpu={} ready_wait_ms={} \
         requested_delay_ms={} observed_delay_ms={}",
        report.vm_id,
        report.host_cpu,
        report.ready_wait_ms,
        report.requested_delay_ms,
        report.observed_delay_ms
    )
}

fn write_repeated_record(record: &str) -> io::Result<()> {
    let mut output = std::io::stdout();
    for copy in 0..EVIDENCE_RECORD_COPIES {
        writeln!(output, "{record}")?;
        output.flush()?;
        if copy + 1 < EVIDENCE_RECORD_COPIES {
            spin_for_duration(EVIDENCE_RECORD_PAUSE);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_configuration_requires_a_complete_valid_placement() {
        assert_eq!(parse_config(None, None, None, None).unwrap(), None);
        assert!(parse_config(Some("1"), Some("3"), Some("12000"), None).is_err());
        assert_eq!(
            parse_config(Some("1"), Some("3"), Some("12000"), Some("30000")).unwrap(),
            Some(GuestRestartConfig {
                vm_id: 1,
                host_cpu: 3,
                delay_ms: 12_000,
                ready_timeout_ms: 30_000,
            })
        );
        assert!(
            validate_config(
                GuestRestartConfig {
                    vm_id: 1,
                    host_cpu: 3,
                    delay_ms: 0,
                    ready_timeout_ms: 30_000,
                },
                4,
            )
            .is_err()
        );
        assert!(
            validate_config(
                GuestRestartConfig {
                    vm_id: 1,
                    host_cpu: 4,
                    delay_ms: 12_000,
                    ready_timeout_ms: 30_000,
                },
                4,
            )
            .is_err()
        );
    }

    #[test]
    fn completion_record_proves_one_running_to_running_reset() {
        let report = GuestRestartReport {
            vm_id: 1,
            host_cpu: 3,
            ready_wait_ms: 425,
            requested_delay_ms: 12_000,
            observed_delay_ms: 12_001,
            before_status: VmStatus::Running,
            after_status: VmStatus::Running,
        };

        validate_report(&report).unwrap();
        assert_eq!(
            format_report(&report),
            "AXVISOR_GUEST_RESTART_COMPLETE schema=1 vm_id=1 host_cpu=3 \
             before_status=running after_status=running reset_count=1"
        );
        assert_eq!(
            format_timing_report(&report),
            "AXVISOR_GUEST_RESTART_TIMING schema=1 vm_id=1 host_cpu=3 \
             ready_wait_ms=425 requested_delay_ms=12000 observed_delay_ms=12001"
        );
    }
}
