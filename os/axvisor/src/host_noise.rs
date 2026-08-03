//! Bounded, explicitly placed host interference for RT isolation experiments.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::{
    io::{self, Write},
    println,
    string::ToString,
    sync::Arc,
    thread::{self, JoinHandle},
    vec,
    vec::Vec,
};

use anyhow::{Context, Result, anyhow, bail};
use ax_std::os::arceos::{api, modules};
use ax_std::sync::Mutex;

const WORKER_STARTING: u8 = 0;
const WORKER_READY: u8 = 1;
const WORKER_FAILED: u8 = 2;
const NANOSECONDS_PER_MILLISECOND: u64 = 1_000_000;

static LAST_REPORT: Mutex<Option<HostNoiseReport>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HostNoiseConfig {
    cpu: usize,
    max_duration_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopReason {
    GuestComplete,
    MaxDuration,
}

impl StopReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::GuestComplete => "guest-complete",
            Self::MaxDuration => "max-duration",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HostNoiseReport {
    requested_cpu: usize,
    affinity_mask: u128,
    observed_cpu_mask: u128,
    max_duration_ms: u64,
    start_ticks: u64,
    end_ticks: u64,
    elapsed_ticks: u64,
    elapsed_ns: u64,
    loop_iterations: u64,
    stop_reason: StopReason,
    per_cpu_observed_wall_ticks: Vec<u64>,
}

/// A running host-noise worker that is stopped after the default guest set exits.
pub(crate) struct HostNoiseTask {
    stop_requested: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<HostNoiseReport>>>,
}

impl HostNoiseTask {
    /// Starts the build-configured worker and waits until its affinity is observed.
    pub(crate) fn start_configured() -> Result<Option<Self>> {
        let Some(config) = configured_host_noise()? else {
            return Ok(None);
        };
        validate_config(config)?;

        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop_requested = stop_requested.clone();
        let worker_state = Arc::new(AtomicU8::new(WORKER_STARTING));
        let observed_worker_state = worker_state.clone();
        let worker = thread::Builder::new()
            .name("axvisor-host-noise".to_string())
            .spawn(move || run_worker(config, worker_stop_requested, worker_state))
            .context("spawn Axvisor host-noise task")?;

        while observed_worker_state.load(Ordering::Acquire) == WORKER_STARTING {
            thread::yield_now();
        }
        if observed_worker_state.load(Ordering::Acquire) == WORKER_FAILED {
            return match join_worker(worker) {
                Err(error) => {
                    Err(error).context("Axvisor host-noise task failed before publishing readiness")
                }
                Ok(_) => {
                    bail!("Axvisor host-noise worker reported failure without returning an error")
                }
            };
        }

        Ok(Some(Self {
            stop_requested,
            worker: Some(worker),
        }))
    }

    /// Stops, joins, validates, and publishes the worker's persisted evidence.
    pub(crate) fn stop_and_publish(mut self) -> Result<()> {
        self.stop_requested.store(true, Ordering::Release);
        let worker = self
            .worker
            .take()
            .context("host-noise worker handle is unavailable")?;
        let report = join_worker(worker)?;

        write_report(&mut std::io::stdout(), &report)
            .context("write Axvisor host-noise completion evidence")?;
        *LAST_REPORT.lock() = Some(report.clone());
        validate_report(&report)
    }
}

/// Writes the completed host-noise record into the durable host RT trace.
pub(crate) fn write_persisted_evidence(output: &mut impl Write) -> io::Result<()> {
    let report = LAST_REPORT.lock().clone();
    if let Some(report) = report {
        write_report(output, &report)?;
    }
    Ok(())
}

fn configured_host_noise() -> Result<Option<HostNoiseConfig>> {
    parse_config(
        option_env!("AXVISOR_HOST_NOISE_CPU"),
        option_env!("AXVISOR_HOST_NOISE_MAX_DURATION_MS"),
    )
}

fn parse_config(
    cpu: Option<&str>,
    max_duration_ms: Option<&str>,
) -> Result<Option<HostNoiseConfig>> {
    match (cpu, max_duration_ms) {
        (None, None) => Ok(None),
        (Some(cpu), Some(max_duration_ms)) => Ok(Some(HostNoiseConfig {
            cpu: cpu
                .parse()
                .with_context(|| format!("parse AXVISOR_HOST_NOISE_CPU `{cpu}`"))?,
            max_duration_ms: max_duration_ms.parse().with_context(|| {
                format!("parse AXVISOR_HOST_NOISE_MAX_DURATION_MS `{max_duration_ms}`")
            })?,
        })),
        _ => bail!(
            "AXVISOR_HOST_NOISE_CPU and AXVISOR_HOST_NOISE_MAX_DURATION_MS must be configured together"
        ),
    }
}

fn validate_config(config: HostNoiseConfig) -> Result<()> {
    let cpu_count = api::sys::ax_get_cpu_num();
    if config.cpu >= cpu_count {
        bail!(
            "host-noise CPU {} is outside the initialized CPU count {}",
            config.cpu,
            cpu_count
        );
    }
    if config.cpu >= u128::BITS as usize {
        bail!(
            "host-noise CPU {} cannot be represented in evidence",
            config.cpu
        );
    }
    if config.max_duration_ms == 0 {
        bail!("host-noise maximum duration must be positive");
    }
    config
        .max_duration_ms
        .checked_mul(NANOSECONDS_PER_MILLISECOND)
        .context("host-noise maximum duration overflows nanoseconds")?;
    Ok(())
}

fn run_worker(
    config: HostNoiseConfig,
    stop_requested: Arc<AtomicBool>,
    worker_state: Arc<AtomicU8>,
) -> Result<HostNoiseReport> {
    let result = prepare_and_run_worker(config, stop_requested, &worker_state);
    if result.is_err() {
        worker_state.store(WORKER_FAILED, Ordering::Release);
    }
    result
}

fn prepare_and_run_worker(
    config: HostNoiseConfig,
    stop_requested: Arc<AtomicBool>,
    worker_state: &AtomicU8,
) -> Result<HostNoiseReport> {
    let affinity = api::task::AxCpuMask::one_shot(config.cpu);
    api::task::ax_set_current_affinity(affinity)
        .map_err(|error| anyhow!("apply singleton host-noise CPU affinity: {error}"))?;
    let actual_cpu = modules::ax_hal::percpu::this_cpu_id();
    if actual_cpu != config.cpu {
        bail!(
            "host-noise affinity requested pCPU{} but readiness ran on pCPU{}",
            config.cpu,
            actual_cpu
        );
    }

    let affinity_mask = 1_u128 << config.cpu;
    println!(
        "AXVISOR_RT_HOST_NOISE_READY schema=1 requested_pcpu={} actual_pcpu={} \
         affinity_mask={:#x} max_duration_ms={} intensity=busy-loop",
        config.cpu, actual_cpu, affinity_mask, config.max_duration_ms
    );
    worker_state.store(WORKER_READY, Ordering::Release);
    Ok(run_busy_loop(config, affinity_mask, stop_requested))
}

fn run_busy_loop(
    config: HostNoiseConfig,
    affinity_mask: u128,
    stop_requested: Arc<AtomicBool>,
) -> HostNoiseReport {
    let cpu_count = api::sys::ax_get_cpu_num();
    let max_duration_ns = config.max_duration_ms * NANOSECONDS_PER_MILLISECOND;
    let max_duration_ticks = modules::ax_hal::time::nanos_to_ticks(max_duration_ns);
    let start_ticks = modules::ax_hal::time::current_ticks();
    let mut previous_ticks = start_ticks;
    let mut previous_cpu = modules::ax_hal::percpu::this_cpu_id();
    let mut per_cpu_observed_wall_ticks = vec![0_u64; cpu_count];
    let mut observed_cpu_mask = 0_u128;
    let mut loop_iterations = 0_u64;

    let (end_ticks, stop_reason) = loop {
        core::hint::spin_loop();
        let current_ticks = modules::ax_hal::time::current_ticks();
        let current_cpu = modules::ax_hal::percpu::this_cpu_id();
        // This interval proves that the worker kept returning on one pCPU and
        // that its wall window covered the capture. It includes time spent
        // preempted by the co-located vCPU, so it is not task CPU runtime.
        let elapsed_on_previous_cpu = current_ticks.wrapping_sub(previous_ticks);
        if let Some(cpu_ticks) = per_cpu_observed_wall_ticks.get_mut(previous_cpu) {
            *cpu_ticks = cpu_ticks.saturating_add(elapsed_on_previous_cpu);
        }
        if current_cpu < u128::BITS as usize {
            observed_cpu_mask |= 1_u128 << current_cpu;
        }
        previous_ticks = current_ticks;
        previous_cpu = current_cpu;
        loop_iterations = loop_iterations.saturating_add(1);

        if stop_requested.load(Ordering::Acquire) {
            break (current_ticks, StopReason::GuestComplete);
        }
        if current_ticks.wrapping_sub(start_ticks) >= max_duration_ticks {
            break (current_ticks, StopReason::MaxDuration);
        }
    };

    let elapsed_ticks = end_ticks.wrapping_sub(start_ticks);
    HostNoiseReport {
        requested_cpu: config.cpu,
        affinity_mask,
        observed_cpu_mask,
        max_duration_ms: config.max_duration_ms,
        start_ticks,
        end_ticks,
        elapsed_ticks,
        elapsed_ns: modules::ax_hal::time::ticks_to_nanos(elapsed_ticks),
        loop_iterations,
        stop_reason,
        per_cpu_observed_wall_ticks,
    }
}

fn join_worker(worker: JoinHandle<Result<HostNoiseReport>>) -> Result<HostNoiseReport> {
    worker
        .join()
        .map_err(|_| anyhow!("Axvisor host-noise task panicked"))?
}

fn validate_report(report: &HostNoiseReport) -> Result<()> {
    if report.stop_reason != StopReason::GuestComplete {
        bail!(
            "host-noise task reached its {} ms limit before the guest set completed",
            report.max_duration_ms
        );
    }
    if report.observed_cpu_mask != report.affinity_mask {
        bail!(
            "host-noise task requested affinity {:#x} but ran on pCPU mask {:#x}",
            report.affinity_mask,
            report.observed_cpu_mask
        );
    }
    Ok(())
}

fn write_report(output: &mut impl Write, report: &HostNoiseReport) -> io::Result<()> {
    writeln!(
        output,
        "AXVISOR_RT_HOST_NOISE schema=1 requested_pcpu={} affinity_mask={:#x} \
         observed_pcpu_mask={:#x} max_duration_ms={} start_ticks={} end_ticks={} \
         elapsed_ticks={} elapsed_ns={} loop_iterations={} stop_reason={} intensity=busy-loop",
        report.requested_cpu,
        report.affinity_mask,
        report.observed_cpu_mask,
        report.max_duration_ms,
        report.start_ticks,
        report.end_ticks,
        report.elapsed_ticks,
        report.elapsed_ns,
        report.loop_iterations,
        report.stop_reason.as_str(),
    )?;
    for (cpu, observed_wall_ticks) in report
        .per_cpu_observed_wall_ticks
        .iter()
        .copied()
        .enumerate()
    {
        if observed_wall_ticks != 0 {
            writeln!(
                output,
                "AXVISOR_RT_HOST_NOISE_PCPU schema=1 pcpu={cpu} \
                 observed_wall_ticks={observed_wall_ticks}"
            )?;
        }
    }
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_configuration_requires_a_complete_positive_pair() {
        assert_eq!(parse_config(None, None).unwrap(), None);
        assert!(parse_config(Some("1"), None).is_err());
        assert!(parse_config(None, Some("1000")).is_err());
        assert_eq!(
            parse_config(Some("3"), Some("180000")).unwrap(),
            Some(HostNoiseConfig {
                cpu: 3,
                max_duration_ms: 180_000,
            })
        );
    }

    #[test]
    fn persisted_report_exposes_placement_runtime_and_stop_reason() {
        let report = HostNoiseReport {
            requested_cpu: 1,
            affinity_mask: 0x2,
            observed_cpu_mask: 0x2,
            max_duration_ms: 180_000,
            start_ticks: 100,
            end_ticks: 220,
            elapsed_ticks: 120,
            elapsed_ns: 5_000,
            loop_iterations: 40,
            stop_reason: StopReason::GuestComplete,
            per_cpu_observed_wall_ticks: vec![0, 120, 0, 0],
        };
        let mut output = Vec::new();

        write_report(&mut output, &report).unwrap();

        let output = std::string::String::from_utf8(output).unwrap();
        assert!(output.contains("requested_pcpu=1"));
        assert!(output.contains("observed_pcpu_mask=0x2"));
        assert!(output.contains("stop_reason=guest-complete"));
        assert!(output.contains("pcpu=1 observed_wall_ticks=120"));
    }
}
