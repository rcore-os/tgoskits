//! Shared CPU frequency policies for ArceOS-based kernels.
//!
//! The SoC driver exposes frequency domains through rdif-cpufreq. This module
//! owns the only policy worker: it samples scheduler load, refreshes hardware
//! limits, and serializes all driver access from kernel callers.

use alloc::{collections::VecDeque, string::String, sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub use rdif_cpufreq::{DomainId, DomainInfo, FrequencyError, FrequencyLimits, OperatingPoint};

use crate::task::sync::{Mutex, WaitQueue};

const POLL_PERIOD: Duration = Duration::from_millis(100);
const UP_THRESHOLD_PCT: u64 = 80;
const DOWN_THRESHOLD_PCT: u64 = 30;

type Device = rdrive::Device<rdif_cpufreq::CpuFreq>;

/// Linux-compatible governor names accepted by the host boot parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Governor {
    Ondemand,
    Performance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Policy {
    Governor(Governor),
    Fixed(u64),
}

/// One coherent view of a domain, acquired by a single worker request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainSnapshot {
    pub info: DomainInfo,
    pub available_opps: Vec<OperatingPoint>,
    pub current_opp: OperatingPoint,
    pub limits: FrequencyLimits,
}

struct Completion<T> {
    result: Mutex<Option<Result<T, FrequencyError>>>,
    wait: WaitQueue,
}

impl<T> Completion<T> {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn finish(&self, result: Result<T, FrequencyError>) {
        *self.result.lock() = Some(result);
        self.wait.notify_all();
    }

    fn wait(&self) -> Result<T, FrequencyError> {
        self.wait.wait_until(|| self.result.lock().is_some());
        self.result
            .lock()
            .take()
            .expect("cpufreq completion disappeared")
    }
}

enum Request {
    Domains(Arc<Completion<Vec<DomainInfo>>>),
    Snapshot(DomainId, Arc<Completion<DomainSnapshot>>),
    AvailableOpps(DomainId, Arc<Completion<Vec<OperatingPoint>>>),
    CurrentOpp(DomainId, Arc<Completion<OperatingPoint>>),
    Limits(DomainId, Arc<Completion<FrequencyLimits>>),
    SetGovernor(Governor, Arc<Completion<()>>),
    SetFixed(DomainId, u64, Arc<Completion<()>>),
}

struct DomainState {
    info: DomainInfo,
    policy: Policy,
    last_error: Option<FrequencyError>,
}

struct CpuBusy {
    cpu: usize,
    last_ns: u64,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static REQUESTS: Mutex<VecDeque<Request>> = Mutex::new(VecDeque::new());
static WAKE: WaitQueue = WaitQueue::new();

fn with_device<T>(
    device: &Device,
    operation: impl FnOnce(&mut rdif_cpufreq::CpuFreq) -> Result<T, FrequencyError>,
) -> Result<T, FrequencyError> {
    let mut driver = device.lock().map_err(|_| FrequencyError::HardwareFailure)?;
    operation(&mut driver)
}

fn submit<T>(
    make_request: impl FnOnce(Arc<Completion<T>>) -> Request,
) -> Result<T, FrequencyError> {
    if !STARTED.load(Ordering::Acquire) {
        if !rdrive::is_initialized() || rdrive::get_one::<rdif_cpufreq::CpuFreq>().is_none() {
            return Err(FrequencyError::NotSupported);
        }
        return Err(FrequencyError::NotReady);
    }
    let completion = Arc::new(Completion::new());
    REQUESTS.lock().push_back(make_request(completion.clone()));
    WAKE.notify_one();
    completion.wait()
}

/// Enumerates the domains registered by the active platform driver.
///
/// This task-context call waits for the policy worker and returns a snapshot.
pub fn domains() -> Result<Vec<DomainInfo>, FrequencyError> {
    submit(Request::Domains)
}

/// Returns a coherent view of one domain and its current restrictions.
pub fn snapshot(domain: DomainId) -> Result<DomainSnapshot, FrequencyError> {
    submit(|completion| Request::Snapshot(domain, completion))
}

/// Returns the currently usable OPPs for one domain.
pub fn available_opps(domain: DomainId) -> Result<Vec<OperatingPoint>, FrequencyError> {
    submit(|completion| Request::AvailableOpps(domain, completion))
}

/// Returns the last fully confirmed OPP for one domain.
pub fn current_opp(domain: DomainId) -> Result<OperatingPoint, FrequencyError> {
    submit(|completion| Request::CurrentOpp(domain, completion))
}

/// Returns the active hardware and thermal frequency limits for one domain.
pub fn limits(domain: DomainId) -> Result<FrequencyLimits, FrequencyError> {
    submit(|completion| Request::Limits(domain, completion))
}

/// Selects a governor for all domains. The call may sleep while the worker
/// applies the performance target.
pub fn set_governor(governor: Governor) -> Result<(), FrequencyError> {
    submit(|completion| Request::SetGovernor(governor, completion))
}

/// Requests an exact available OPP for one domain. The call may sleep while
/// the worker applies and confirms the driver transition.
pub fn set_fixed_frequency(domain: DomainId, frequency_hz: u64) -> Result<(), FrequencyError> {
    submit(|completion| Request::SetFixed(domain, frequency_hz, completion))
}

fn parse_governor(bootargs: Option<&str>) -> Governor {
    let value = bootargs.and_then(|args| {
        args.split_whitespace()
            .filter_map(|arg| arg.strip_prefix("cpufreq.default_governor="))
            .next_back()
    });
    match value {
        None | Some("ondemand") => Governor::Ondemand,
        Some("performance") => Governor::Performance,
        Some(other) => {
            warn!("cpufreq: invalid cpufreq.default_governor={other}; using ondemand");
            Governor::Ondemand
        }
    }
}

fn domain_snapshot(
    driver: &mut rdif_cpufreq::CpuFreq,
    domain: DomainId,
) -> Result<DomainSnapshot, FrequencyError> {
    let info = driver
        .domains()?
        .into_iter()
        .find(|info| info.id == domain)
        .ok_or(FrequencyError::InvalidDomain)?;
    Ok(DomainSnapshot {
        info,
        available_opps: driver.available_opps(domain)?,
        current_opp: driver.current_opp(domain)?,
        limits: driver.limits(domain)?,
    })
}

fn handle_request(
    request: Request,
    device: &Device,
    states: &mut [DomainState],
    refresh: Result<(), FrequencyError>,
) {
    match request {
        Request::Domains(done) => done.finish(with_device(device, |driver| driver.domains())),
        Request::Snapshot(domain, done) => {
            done.finish(with_device(device, |driver| {
                domain_snapshot(driver, domain)
            }));
        }
        Request::AvailableOpps(domain, done) => {
            done.finish(with_device(device, |driver| driver.available_opps(domain)));
        }
        Request::CurrentOpp(domain, done) => {
            done.finish(with_device(device, |driver| driver.current_opp(domain)));
        }
        Request::Limits(domain, done) => {
            done.finish(with_device(device, |driver| driver.limits(domain)));
        }
        Request::SetGovernor(governor, done) => {
            let result = refresh.and_then(|()| {
                for state in states.iter_mut() {
                    state.policy = Policy::Governor(governor);
                }
                if governor == Governor::Performance {
                    apply_policies(device, states, &[], true)
                } else {
                    Ok(())
                }
            });
            done.finish(result);
        }
        Request::SetFixed(domain, hz, done) => {
            let result = refresh.and_then(|()| {
                let state = states
                    .iter_mut()
                    .find(|state| state.info.id == domain)
                    .ok_or(FrequencyError::InvalidDomain)?;
                with_device(device, |driver| driver.set_frequency(domain, hz))?;
                state.policy = Policy::Fixed(hz);
                Ok(())
            });
            done.finish(result);
        }
    }
}

fn eligible_opps(
    driver: &mut rdif_cpufreq::CpuFreq,
    domain: DomainId,
) -> Result<Vec<OperatingPoint>, FrequencyError> {
    let limits = driver.limits(domain)?;
    let mut opps = driver.available_opps(domain)?;
    opps.retain(|opp| (limits.min_hz..=limits.max_hz).contains(&opp.frequency_hz));
    opps.sort_unstable_by_key(|opp| opp.frequency_hz);
    opps.dedup_by_key(|opp| opp.frequency_hz);
    if opps.is_empty() {
        return Err(FrequencyError::OppUnavailable);
    }
    Ok(opps)
}

fn next_ondemand_frequency(
    percents: &[u64],
    current_hz: u64,
    opps: &[OperatingPoint],
    priming: bool,
) -> Option<u64> {
    if priming || percents.is_empty() {
        return None;
    }
    let current = opps.iter().position(|opp| opp.frequency_hz == current_hz)?;
    if percents.iter().any(|&pct| pct >= UP_THRESHOLD_PCT) {
        return opps.last().map(|opp| opp.frequency_hz);
    }
    if percents.iter().all(|&pct| pct < DOWN_THRESHOLD_PCT) && current > 0 {
        return Some(opps[current - 1].frequency_hz);
    }
    None
}

fn target_frequency(
    driver: &mut rdif_cpufreq::CpuFreq,
    state: &DomainState,
    samples: &[(usize, u64)],
    priming: bool,
) -> Result<Option<u64>, FrequencyError> {
    let domain = state.info.id;
    let opps = eligible_opps(driver, domain)?;
    let current = driver.current_opp(domain)?.frequency_hz;
    let target = match state.policy {
        Policy::Governor(Governor::Performance) => opps.last().map(|opp| opp.frequency_hz),
        Policy::Fixed(requested) => opps
            .iter()
            .rev()
            .find(|opp| opp.frequency_hz <= requested)
            .or_else(|| opps.first())
            .map(|opp| opp.frequency_hz),
        Policy::Governor(Governor::Ondemand) => {
            let percents: Vec<u64> = state
                .info
                .cpu_ids
                .iter()
                .filter_map(|cpu| {
                    samples
                        .iter()
                        .find(|(id, _)| id == cpu)
                        .map(|(_, pct)| *pct)
                })
                .collect();
            next_ondemand_frequency(&percents, current, &opps, priming)
        }
    };
    Ok(target.filter(|&hz| hz != current))
}

fn apply_policies(
    device: &Device,
    states: &mut [DomainState],
    samples: &[(usize, u64)],
    priming: bool,
) -> Result<(), FrequencyError> {
    let mut first_error = None;
    for state in states {
        let result = with_device(device, |driver| {
            if let Some(target) = target_frequency(driver, state, samples, priming)? {
                driver.set_frequency(state.info.id, target)?;
            }
            Ok(())
        });
        match result {
            Ok(()) => state.last_error = None,
            Err(error) => {
                if state.last_error != Some(error) && error != FrequencyError::NotReady {
                    warn!(
                        "cpufreq: domain {:?} policy update failed: {error}",
                        state.info.id
                    );
                }
                state.last_error = Some(error);
                first_error.get_or_insert(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn sample_load(busy: &mut [CpuBusy], last_poll_ns: &mut Option<u64>) -> (Vec<(usize, u64)>, bool) {
    let now_ns = axklib::time::monotonic_nanos();
    let priming = last_poll_ns.is_none();
    let window_ns = now_ns
        .saturating_sub(last_poll_ns.replace(now_ns).unwrap_or(now_ns))
        .max(1);
    let mut samples = Vec::with_capacity(busy.len());
    for entry in busy {
        let Ok(cpu) = u32::try_from(entry.cpu) else {
            continue;
        };
        let Ok(runtime_ns) =
            crate::task::sched::cpu_busy_runtime_ns(crate::task::sched::CpuId::new(cpu))
        else {
            continue;
        };
        let delta_ns = runtime_ns.saturating_sub(entry.last_ns);
        entry.last_ns = runtime_ns;
        let pct = (delta_ns.saturating_mul(100) / window_ns).min(100);
        samples.push((entry.cpu, pct));
    }
    (samples, priming)
}

fn run_worker(device: Device, infos: Vec<DomainInfo>, initial: Governor) {
    let mut states: Vec<_> = infos
        .into_iter()
        .map(|info| DomainState {
            info,
            policy: Policy::Governor(initial),
            last_error: None,
        })
        .collect();
    let mut busy = Vec::new();
    for state in &states {
        for &cpu in &state.info.cpu_ids {
            if !busy.iter().any(|entry: &CpuBusy| entry.cpu == cpu) {
                busy.push(CpuBusy { cpu, last_ns: 0 });
            }
        }
    }
    let mut last_poll_ns = None;
    let mut last_refresh_error = None;
    loop {
        let refresh = with_device(&device, |driver| driver.refresh_limits());
        match refresh {
            Ok(()) => last_refresh_error = None,
            Err(error) => {
                if last_refresh_error != Some(error) {
                    warn!("cpufreq: hardware limit refresh failed: {error}");
                }
                last_refresh_error = Some(error);
            }
        }
        loop {
            let request = REQUESTS.lock().pop_front();
            let Some(request) = request else { break };
            handle_request(request, &device, &mut states, refresh);
        }
        let (samples, priming) = sample_load(&mut busy, &mut last_poll_ns);
        if refresh.is_ok() {
            let _ = apply_policies(&device, &mut states, &samples, priming);
        }
        WAKE.wait_timeout_until(POLL_PERIOD, || !REQUESTS.lock().is_empty());
    }
}

/// Starts the shared worker after every host CPU is online, before the OS app.
/// Only host boot arguments are consulted; guest command lines never reach here.
pub(crate) fn start_from_host_bootargs(bootargs: Option<&str>) {
    if !rdrive::is_initialized() {
        return;
    }
    let Some(device) = rdrive::get_one::<rdif_cpufreq::CpuFreq>() else {
        return;
    };
    let infos = match with_device(&device, |driver| driver.domains()) {
        Ok(infos) if !infos.is_empty() => infos,
        Ok(_) => return,
        Err(error) => {
            warn!("cpufreq: cannot enumerate domains at startup: {error}");
            return;
        }
    };
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let governor = parse_governor(bootargs);
    info!(
        "cpufreq: starting shared {governor:?} governor for {} domains",
        infos.len()
    );
    crate::thread::builder(String::from("cpufreq-gov"))
        .spawn(move || run_worker(device, infos, governor))
        .expect("failed to start cpufreq worker");
    if governor == Governor::Performance
        && let Err(error) = set_governor(governor)
    {
        warn!("cpufreq: boot performance policy was not fully applied: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::{Governor, OperatingPoint, next_ondemand_frequency, parse_governor};

    #[test]
    fn boot_governor_selection() {
        assert_eq!(parse_governor(None), Governor::Ondemand);
        assert_eq!(parse_governor(Some("console=ttyS0")), Governor::Ondemand);
        assert_eq!(
            parse_governor(Some("cpufreq.default_governor=ondemand")),
            Governor::Ondemand
        );
        assert_eq!(
            parse_governor(Some(
                "root=/dev/mmcblk0 cpufreq.default_governor=performance"
            )),
            Governor::Performance
        );
        assert_eq!(
            parse_governor(Some("cpufreq.default_governor=bad")),
            Governor::Ondemand
        );
    }

    #[test]
    fn ondemand_uses_busiest_cpu_and_decays_one_opp() {
        let opps = [408, 816, 1200].map(|mhz| OperatingPoint {
            frequency_hz: mhz * 1_000_000,
            voltage_uv: None,
        });
        assert_eq!(
            next_ondemand_frequency(&[99, 0], 816_000_000, &opps, true),
            None
        );
        assert_eq!(
            next_ondemand_frequency(&[99, 0], 816_000_000, &opps, false),
            Some(1_200_000_000)
        );
        assert_eq!(
            next_ondemand_frequency(&[10, 20], 1_200_000_000, &opps, false),
            Some(816_000_000)
        );
        assert_eq!(
            next_ondemand_frequency(&[], 816_000_000, &opps, false),
            None
        );
    }
}
