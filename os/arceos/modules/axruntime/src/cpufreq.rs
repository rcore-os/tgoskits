//! Shared CPU frequency policies for ArceOS-based kernels.
//!
//! The SoC driver exposes frequency domains through rdif-cpufreq. This module
//! owns the only policy worker: it samples scheduler load, refreshes hardware
//! limits, and serializes all driver access from kernel callers.

use alloc::{collections::VecDeque, string::String, sync::Arc, vec, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub use rdif_cpufreq::{DomainId, DomainInfo, FrequencyError, FrequencyLimits, OperatingPoint};

use crate::task::sync::{Mutex, WaitQueue};

const POLL_PERIOD: Duration = Duration::from_millis(100);
const UP_THRESHOLD_PCT: u64 = 80;
const DOWN_THRESHOLD_PCT: u64 = 30;
/// Highest normalized performance published to a guest (`qemu,virtual-cpufreq`).
pub const GUEST_MAX_PERFORMANCE: u32 = 1024;
const GUEST_PERFORMANCE_SPAN: usize = (GUEST_MAX_PERFORMANCE - 1) as usize;

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
    pub all_opps: Vec<OperatingPoint>,
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
    AllOpps(DomainId, Arc<Completion<Vec<OperatingPoint>>>),
    CurrentOpp(DomainId, Arc<Completion<OperatingPoint>>),
    Limits(DomainId, Arc<Completion<FrequencyLimits>>),
    SetGovernor(Governor, Arc<Completion<()>>),
    SetFixed(DomainId, u64, Arc<Completion<()>>),
}

struct DomainState {
    info: DomainInfo,
    policy: Policy,
    last_error: Option<FrequencyError>,
    last_fixed_adjustment: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestHint {
    vm_id: usize,
    run_generation: u64,
    vcpu_id: usize,
    domain: usize,
    performance: u32,
    suspended: bool,
}

impl GuestHint {
    fn belongs_to(&self, vm_id: usize, run_generation: u64) -> bool {
        self.vm_id == vm_id && self.run_generation == run_generation
    }
}

/// One VM execution generation admitted to the host aggregation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestRun {
    vm_id: usize,
    run_generation: u64,
    /// A retiring run no longer contributes hints. Its binding stays until the
    /// run is unbound so guest reads keep being served while its vCPUs are
    /// still executing guest code.
    retiring: bool,
}

impl GuestRun {
    fn belongs_to(&self, vm_id: usize, run_generation: u64) -> bool {
        self.vm_id == vm_id && self.run_generation == run_generation
    }
}

/// Run-scoped guest requests owned by the single host policy worker.
///
/// This registry is the only owner of the "may this run influence host policy"
/// decision, so the guest MMIO path never derives it from VM lifecycle state.
#[derive(Default)]
struct GuestRequests {
    hints: Vec<GuestHint>,
    runs: Vec<GuestRun>,
}

impl GuestRequests {
    const fn new() -> Self {
        Self {
            hints: Vec::new(),
            runs: Vec::new(),
        }
    }

    /// Admits one run generation. Binding is idempotent.
    fn bind(&mut self, vm_id: usize, run_generation: u64) {
        if !self.serves(vm_id, run_generation) {
            self.runs.push(GuestRun {
                vm_id,
                run_generation,
                retiring: false,
            });
        }
    }

    /// Reports whether this generation is still bound to the host worker.
    ///
    /// A retiring run stays bound: Linux reads the current performance on every
    /// scheduler tick, and an MMIO error on that path faults the vCPU.
    fn serves(&self, vm_id: usize, run_generation: u64) -> bool {
        self.runs
            .iter()
            .any(|run| run.belongs_to(vm_id, run_generation))
    }

    /// Retires one generation: its hints stop contributing to host policy
    /// immediately, while the binding stays until the run is unbound.
    fn retire(&mut self, vm_id: usize, run_generation: u64) {
        self.hints
            .retain(|hint| !hint.belongs_to(vm_id, run_generation));
        if let Some(run) = self
            .runs
            .iter_mut()
            .find(|run| run.belongs_to(vm_id, run_generation))
        {
            run.retiring = true;
        }
    }

    /// Releases one generation once no vCPU of that run can access the device.
    fn unbind(&mut self, vm_id: usize, run_generation: u64) {
        self.hints
            .retain(|hint| !hint.belongs_to(vm_id, run_generation));
        self.runs
            .retain(|run| !run.belongs_to(vm_id, run_generation));
    }

    /// Stores one normalized hint.
    ///
    /// A retiring generation accepts the request without recording it: the vCPU
    /// that issued it may still be running guest code during teardown, and
    /// reporting an MMIO error there would fault that vCPU and misattribute the
    /// stop reason.
    fn publish(
        &mut self,
        vm_id: usize,
        run_generation: u64,
        vcpu_id: usize,
        domain: usize,
        performance: u32,
    ) -> Result<(), FrequencyError> {
        let Some(run) = self
            .runs
            .iter()
            .find(|run| run.belongs_to(vm_id, run_generation))
        else {
            return Err(FrequencyError::NotReady);
        };
        if run.retiring {
            return Ok(());
        }
        let performance = performance.clamp(1, GUEST_MAX_PERFORMANCE);
        if let Some(existing) = self.hints.iter_mut().find(|hint| {
            hint.vm_id == vm_id
                && hint.run_generation == run_generation
                && hint.vcpu_id == vcpu_id
                && hint.domain == domain
        }) {
            existing.performance = performance;
        } else {
            self.hints.push(GuestHint {
                vm_id,
                run_generation,
                vcpu_id,
                domain,
                performance,
                suspended: false,
            });
        }
        Ok(())
    }

    /// Temporarily excludes one run from aggregation while retaining its values.
    fn suspend(&mut self, vm_id: usize, run_generation: u64) {
        for hint in self
            .hints
            .iter_mut()
            .filter(|hint| hint.belongs_to(vm_id, run_generation))
        {
            hint.suspended = true;
        }
    }

    /// Re-enables one run's retained hints.
    fn resume(&mut self, vm_id: usize, run_generation: u64) {
        for hint in self
            .hints
            .iter_mut()
            .filter(|hint| hint.belongs_to(vm_id, run_generation))
        {
            hint.suspended = false;
        }
    }
}

struct CpuBusy {
    cpu: usize,
    last_ns: u64,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static DOMAIN_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static REQUESTS: Mutex<VecDeque<Request>> = Mutex::new(VecDeque::new());
static GUEST_REQUESTS: Mutex<GuestRequests> = Mutex::new(GuestRequests::new());
static GUEST_DOMAIN_EFFECTIVE: Mutex<Vec<Option<u32>>> = Mutex::new(Vec::new());
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

/// Returns the hardware-validated OPP table before dynamic limits.
pub fn all_opps(domain: DomainId) -> Result<Vec<OperatingPoint>, FrequencyError> {
    submit(|completion| Request::AllOpps(domain, completion))
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

/// Requests an exact currently available OPP for one domain. The call may
/// sleep while the worker applies and confirms the driver transition. Later
/// thermal or dependent-domain limits can force another OPP; use
/// [`current_opp`] or [`snapshot`] to observe the delivered frequency.
pub fn set_fixed_frequency(domain: DomainId, frequency_hz: u64) -> Result<(), FrequencyError> {
    submit(|completion| Request::SetFixed(domain, frequency_hz, completion))
}

/// Returns the number of host frequency domains known to the policy worker.
pub fn domain_count() -> Option<usize> {
    let count = DOMAIN_COUNT.load(Ordering::Acquire);
    (count != 0).then_some(count)
}

/// Seeds the last confirmed normalized value observed while a VM's OPP table
/// is materialized. This lets a guest read `cur_perf` during cpufreq policy
/// discovery, before its first `SET_PERF` write, without inventing a value.
pub fn seed_guest_domain_performance(
    domain: usize,
    performance: u32,
) -> Result<(), FrequencyError> {
    if !STARTED.load(Ordering::Acquire) {
        return Err(FrequencyError::NotReady);
    }
    let mut effective = GUEST_DOMAIN_EFFECTIVE.lock();
    let Some(value) = effective.get_mut(domain) else {
        return Err(FrequencyError::InvalidDomain);
    };
    *value = Some(performance.clamp(1, GUEST_MAX_PERFORMANCE));
    Ok(())
}

/// Marks one VM execution generation as eligible to publish/read hints.
pub fn bind_guest_run(vm_id: usize, run_generation: u64) {
    GUEST_REQUESTS.lock().bind(vm_id, run_generation);
}

/// Publishes or replaces one normalized guest hint without waiting for a
/// hardware transition. The policy worker applies the maximum hint per host
/// domain after refreshing thermal and board-specific OPP limits.
pub fn publish_guest_hint(
    vm_id: usize,
    run_generation: u64,
    vcpu_id: usize,
    domain: usize,
    performance: u32,
) -> Result<(), FrequencyError> {
    if !STARTED.load(Ordering::Acquire) {
        return Err(FrequencyError::NotReady);
    }
    if domain >= DOMAIN_COUNT.load(Ordering::Acquire) {
        return Err(FrequencyError::InvalidDomain);
    }
    GUEST_REQUESTS
        .lock()
        .publish(vm_id, run_generation, vcpu_id, domain, performance)?;
    WAKE.notify_one();
    Ok(())
}

/// Retires one exact VM execution generation.
///
/// Stop calls this before teardown, so a failed cleanup cannot leave a retired
/// run raising the host CPU-frequency floor. The binding is kept until
/// [`unbind_guest_run`], because the guest may still read `CUR_PERF` while its
/// vCPUs are stopping.
pub fn retire_guest_run(vm_id: usize, run_generation: u64) {
    GUEST_REQUESTS.lock().retire(vm_id, run_generation);
    WAKE.notify_one();
}

/// Releases one exact VM execution generation.
///
/// The caller must only use this once every vCPU of the run has stopped, so no
/// device access can observe the missing binding.
pub fn unbind_guest_run(vm_id: usize, run_generation: u64) {
    GUEST_REQUESTS.lock().unbind(vm_id, run_generation);
    WAKE.notify_one();
}

/// Temporarily excludes one run from aggregation while retaining its desired
/// values for resume.
pub fn suspend_guest_hints(vm_id: usize, run_generation: u64) {
    GUEST_REQUESTS.lock().suspend(vm_id, run_generation);
    WAKE.notify_one();
}

/// Re-enables one run's retained guest hints.
pub fn resume_guest_hints(vm_id: usize, run_generation: u64) {
    GUEST_REQUESTS.lock().resume(vm_id, run_generation);
    WAKE.notify_one();
}

/// Returns the latest host-confirmed normalized value for one exact guest vCPU.
///
/// Host policy or limit-refresh failures keep this last confirmed value, and a
/// retiring run keeps being served until it is unbound. The Linux
/// `virt-cpufreq` driver reads `CUR_PERF` on every scheduler tick, and a typed
/// error on that path becomes a vCPU fault; a recoverable host failure must not
/// stop the guest. `SET_PERF` still rejects an unbound run.
pub fn guest_hint_performance(
    vm_id: usize,
    run_generation: u64,
    vcpu_id: usize,
    domain: usize,
) -> Result<Option<u32>, FrequencyError> {
    if !GUEST_REQUESTS.lock().serves(vm_id, run_generation) {
        return Err(FrequencyError::NotReady);
    }
    let effective = GUEST_DOMAIN_EFFECTIVE.lock().get(domain).copied().flatten();
    if effective.is_none() {
        return Err(FrequencyError::NotReady);
    }
    // A current-performance read is useful during Linux policy probing, before
    // the first SET_PERF write creates a per-run hint. The domain value is
    // shared by all vCPUs, while the exact run registration above prevents a
    // stale device from observing a later generation.
    let _ = vcpu_id;
    Ok(effective)
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
        all_opps: driver.all_opps(domain)?,
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
        Request::AllOpps(domain, done) => {
            done.finish(with_device(device, |driver| driver.all_opps(domain)));
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
                    state.last_fixed_adjustment = None;
                }
                if governor == Governor::Performance {
                    apply_policies(device, states, &[], true, &[])
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
                state.last_fixed_adjustment = None;
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
    opps.sort_unstable_by_key(|opp| opp.frequency_hz);
    opps.dedup_by_key(|opp| opp.frequency_hz);
    opps.retain(|opp| (limits.min_hz..=limits.max_hz).contains(&opp.frequency_hz));
    if opps.is_empty() {
        return Err(FrequencyError::OppUnavailable);
    }
    Ok(opps)
}

/// Returns the complete board OPP table before dynamic thermal or
/// dependent-domain limits are applied. Guest performance values use this
/// stable scale so a later thermal ceiling produces a lower `cur_perf` value
/// instead of renormalizing the reduced table back to 1024.
fn all_reference_opps(
    driver: &mut rdif_cpufreq::CpuFreq,
    domain: DomainId,
) -> Result<Vec<OperatingPoint>, FrequencyError> {
    let mut opps = driver.all_opps(domain)?;
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

fn fixed_target(requested: u64, opps: &[OperatingPoint]) -> u64 {
    // A thermal cap can remove the requested OPP. A dependent domain may
    // instead raise the minimum above it; that hardware floor takes priority.
    opps.iter()
        .rev()
        .find(|opp| opp.frequency_hz <= requested)
        .or_else(|| opps.first())
        .expect("eligible OPPs were checked")
        .frequency_hz
}

struct PolicyDecision {
    target: Option<u64>,
    fixed_adjustment: Option<(u64, u64)>,
}

fn target_frequency(
    driver: &mut rdif_cpufreq::CpuFreq,
    state: &DomainState,
    domain_index: usize,
    samples: &[(usize, u64)],
    priming: bool,
    guest_hints: &[GuestHint],
) -> Result<PolicyDecision, FrequencyError> {
    let domain = state.info.id;
    let reference_opps = all_reference_opps(driver, domain)?;
    let opps = eligible_opps(driver, domain)?;
    let current = driver.current_opp(domain)?.frequency_hz;
    let target = match state.policy {
        Policy::Governor(Governor::Performance) => opps.last().map(|opp| opp.frequency_hz),
        Policy::Fixed(requested) => Some(fixed_target(requested, &opps)),
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
    let guest_floor = guest_floor_with_reference(guest_hints, domain_index, &opps, &reference_opps);
    let target = match (target, guest_floor) {
        (Some(policy), Some(guest)) => Some(policy.max(guest)),
        (None, Some(guest)) => Some(guest),
        (policy, None) => policy,
    };
    let fixed_adjustment = match (state.policy, target) {
        (Policy::Fixed(requested), Some(applied)) if applied != requested => {
            Some((requested, applied))
        }
        _ => None,
    };
    Ok(PolicyDecision {
        target: target.filter(|&hz| hz != current),
        fixed_adjustment,
    })
}

/// Builds the strictly increasing guest performance table for one OPP list.
///
/// `opps` must already be sorted by frequency and de-duplicated. Index `0` is
/// always `1` and the last index is always [`GUEST_MAX_PERFORMANCE`].
/// Intermediate values use `1 + (index * 1023) / (n - 1)` so the same formula
/// owns both table materialization and later `CUR_PERF` read-back.
pub fn guest_performance_table(opps: &[OperatingPoint]) -> Vec<u32> {
    if opps.len() <= 1 {
        return vec![GUEST_MAX_PERFORMANCE];
    }
    (0..opps.len())
        .map(|index| guest_performance_for_index(index, opps.len()))
        .collect()
}

fn guest_performance_for_index(index: usize, count: usize) -> u32 {
    if count <= 1 {
        return GUEST_MAX_PERFORMANCE;
    }
    1 + ((index * GUEST_PERFORMANCE_SPAN) / (count - 1)) as u32
}

/// Maps a guest table value back to the same OPP index that produced it.
///
/// `opps` must already be sorted by frequency. The inverse takes the largest
/// index whose table value is `<= performance`, so every value produced by
/// [`guest_performance_table`] round-trips to its original index.
fn guest_performance_index(performance: u32, opps: &[OperatingPoint]) -> usize {
    if opps.is_empty() {
        return 0;
    }
    let performance = performance.clamp(1, GUEST_MAX_PERFORMANCE);
    (0..opps.len())
        .rev()
        .find(|&index| performance >= guest_performance_for_index(index, opps.len()))
        .unwrap_or(0)
}

fn normalized_performance(frequency_hz: u64, opps: &[OperatingPoint]) -> u32 {
    if opps.len() <= 1 {
        return GUEST_MAX_PERFORMANCE;
    }
    let index = opps
        .iter()
        .rposition(|opp| opp.frequency_hz <= frequency_hz)
        .unwrap_or(0);
    guest_performance_for_index(index, opps.len())
}

fn guest_floor_with_reference(
    hints: &[GuestHint],
    domain: usize,
    opps: &[OperatingPoint],
    reference_opps: &[OperatingPoint],
) -> Option<u64> {
    if opps.is_empty() || reference_opps.is_empty() {
        return None;
    }
    hints
        .iter()
        .filter(|hint| hint.domain == domain && !hint.suspended)
        .map(|hint| {
            let index = guest_performance_index(hint.performance, reference_opps);
            let requested = reference_opps[index].frequency_hz;
            opps.iter()
                .find(|opp| opp.frequency_hz >= requested)
                .or_else(|| opps.last())
                .expect("eligible OPPs are non-empty")
                .frequency_hz
        })
        .max()
}

fn publish_effective_performance(
    domain: usize,
    frequency_hz: u64,
    reference_opps: &[OperatingPoint],
) {
    let effective = normalized_performance(frequency_hz, reference_opps);
    if let Some(value) = GUEST_DOMAIN_EFFECTIVE.lock().get_mut(domain) {
        *value = Some(effective);
    }
}

fn apply_policies(
    device: &Device,
    states: &mut [DomainState],
    samples: &[(usize, u64)],
    priming: bool,
    guest_hints: &[GuestHint],
) -> Result<(), FrequencyError> {
    let mut first_error = None;
    for (domain_index, state) in states.iter_mut().enumerate() {
        let result = with_device(device, |driver| {
            let decision =
                target_frequency(driver, state, domain_index, samples, priming, guest_hints)?;
            if let Some(target) = decision.target {
                driver.set_frequency(state.info.id, target)?;
            }
            let current = driver.current_opp(state.info.id)?.frequency_hz;
            let reference_opps = all_reference_opps(driver, state.info.id)?;
            Ok((decision.fixed_adjustment, current, reference_opps))
        });
        match result {
            Ok((adjustment, current, reference_opps)) => {
                publish_effective_performance(domain_index, current, &reference_opps);
                if let Some((requested, applied)) = adjustment {
                    if state.last_fixed_adjustment != Some(applied) {
                        let limit = if applied > requested {
                            "minimum frequency limit"
                        } else {
                            "maximum frequency limit"
                        };
                        warn!(
                            "cpufreq: domain {:?} fixed request {requested} Hz constrained to \
                             {applied} Hz by {limit}",
                            state.info.id
                        );
                    }
                    state.last_fixed_adjustment = Some(applied);
                } else {
                    state.last_fixed_adjustment = None;
                }
                state.last_error = None;
            }
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
    let (window_ns, priming) = observe_window(now_ns, *last_poll_ns);
    *last_poll_ns = Some(now_ns);
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
        let pct = busy_percent(delta_ns, window_ns);
        samples.push((entry.cpu, pct));
    }
    (samples, priming)
}

/// Returns the elapsed sampling window and whether this is the first sample.
///
/// Scheduler wakeups can make the worker run earlier or later than its target
/// period. The load ratio must use that actual elapsed interval. A backwards
/// clock value is treated as a one-nanosecond interval so a malformed sample
/// cannot divide by zero or retain a stale elapsed window.
fn observe_window(now_ns: u64, last_ns: Option<u64>) -> (u64, bool) {
    let priming = last_ns.is_none();
    let window_ns = now_ns.saturating_sub(last_ns.unwrap_or(now_ns)).max(1);
    (window_ns, priming)
}

/// Converts busy runtime accumulated during one sampling window to a bounded
/// percentage. Saturating arithmetic keeps a corrupt counter from wrapping.
fn busy_percent(delta_ns: u64, window_ns: u64) -> u64 {
    (delta_ns.saturating_mul(100) / window_ns.max(1)).min(100)
}

fn run_worker(device: Device, infos: Vec<DomainInfo>, initial: Governor) {
    let mut states: Vec<_> = infos
        .into_iter()
        .map(|info| DomainState {
            info,
            policy: Policy::Governor(initial),
            last_error: None,
            last_fixed_adjustment: None,
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
        let guest_hints = GUEST_REQUESTS.lock().hints.clone();
        if refresh.is_ok() {
            let _ = apply_policies(&device, &mut states, &samples, priming, &guest_hints);
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
    DOMAIN_COUNT.store(infos.len(), Ordering::Release);
    *GUEST_DOMAIN_EFFECTIVE.lock() = vec![None; infos.len()];
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
    use super::{
        FrequencyError, GUEST_MAX_PERFORMANCE, Governor, GuestHint, GuestRequests, OperatingPoint,
        busy_percent, fixed_target, guest_floor_with_reference, guest_performance_index,
        guest_performance_table, next_ondemand_frequency, normalized_performance, observe_window,
        parse_governor,
    };

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
    fn busy_percent_uses_the_actual_elapsed_window() {
        assert_eq!(busy_percent(50_000_000, 100_000_000), 50);
        assert_eq!(busy_percent(100_000_000, 200_000_000), 50);
        assert_eq!(busy_percent(200_000_000, 100_000_000), 100);
        assert_eq!(busy_percent(1, 0), 100);
        assert_eq!(busy_percent(u64::MAX, u64::MAX), 1);
    }

    #[test]
    fn observe_window_tracks_priming_and_clock_jumps() {
        assert_eq!(observe_window(10, None), (1, true));
        assert_eq!(observe_window(110, Some(10)), (100, false));
        assert_eq!(observe_window(90, Some(110)), (1, false));
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

    #[test]
    fn guest_performance_is_normalized_against_reference_opps() {
        let opps = [408, 816, 1200].map(|mhz| OperatingPoint {
            frequency_hz: mhz * 1_000_000,
            voltage_uv: None,
        });
        assert_eq!(normalized_performance(408_000_000, &opps), 1);
        assert_eq!(normalized_performance(816_000_000, &opps), 512);
        assert_eq!(normalized_performance(1_200_000_000, &opps), 1024);
        assert_eq!(normalized_performance(500_000_000, &opps), 1);
        assert_eq!(normalized_performance(816_000_000, &opps[..2]), 1024);
    }

    #[test]
    fn guest_floor_aggregates_maximum_and_ignores_suspended_runs() {
        let opps = [408, 816, 1200].map(|mhz| OperatingPoint {
            frequency_hz: mhz * 1_000_000,
            voltage_uv: None,
        });
        let hints = [
            GuestHint {
                vm_id: 1,
                run_generation: 3,
                vcpu_id: 0,
                domain: 1,
                performance: 256,
                suspended: false,
            },
            GuestHint {
                vm_id: 2,
                run_generation: 7,
                vcpu_id: 0,
                domain: 1,
                performance: 1024,
                suspended: true,
            },
            GuestHint {
                vm_id: 3,
                run_generation: 9,
                vcpu_id: 0,
                domain: 1,
                performance: 512,
                suspended: false,
            },
        ];
        assert_eq!(
            guest_floor_with_reference(&hints, 1, &opps, &opps),
            Some(816_000_000)
        );
        assert_eq!(
            guest_floor_with_reference(
                &[GuestHint {
                    vm_id: 4,
                    run_generation: 1,
                    vcpu_id: 0,
                    domain: 1,
                    performance: 1,
                    suspended: false,
                }],
                1,
                &opps,
                &opps,
            ),
            Some(408_000_000)
        );
        assert_eq!(guest_floor_with_reference(&hints, 0, &opps, &opps), None);
    }

    #[test]
    fn guest_table_entries_round_trip_to_the_same_opp_index() {
        for count in 2..=64 {
            let opps: Vec<_> = (1..=count as u64)
                .map(|i| OperatingPoint {
                    frequency_hz: i * 100_000_000,
                    voltage_uv: None,
                })
                .collect();
            let table = guest_performance_table(&opps);
            assert_eq!(table.first().copied(), Some(1));
            assert_eq!(table.last().copied(), Some(GUEST_MAX_PERFORMANCE));
            for (index, performance) in table.iter().copied().enumerate() {
                assert_eq!(
                    guest_performance_index(performance, &opps),
                    index,
                    "count={count} index={index} performance={performance}"
                );
                assert_eq!(
                    guest_floor_with_reference(
                        &[GuestHint {
                            vm_id: 1,
                            run_generation: 1,
                            vcpu_id: 0,
                            domain: 0,
                            performance,
                            suspended: false,
                        }],
                        0,
                        &opps,
                        &opps,
                    ),
                    Some(opps[index].frequency_hz),
                    "count={count} index={index} performance={performance}"
                );
                assert_eq!(
                    normalized_performance(opps[index].frequency_hz, &opps),
                    performance
                );
            }
        }
    }

    #[test]
    fn thermal_ceiling_preserves_reference_scale_for_guest_readback() {
        let reference = [408, 816, 1200].map(|mhz| OperatingPoint {
            frequency_hz: mhz * 1_000_000,
            voltage_uv: None,
        });
        let limited = &reference[..2];
        assert_eq!(normalized_performance(816_000_000, &reference), 512);
        assert_eq!(
            guest_floor_with_reference(
                &[GuestHint {
                    vm_id: 1,
                    run_generation: 1,
                    vcpu_id: 0,
                    domain: 0,
                    performance: 512,
                    suspended: false,
                }],
                0,
                limited,
                &reference,
            ),
            Some(816_000_000)
        );
    }

    #[test]
    fn fixed_request_obeys_new_thermal_ceiling_and_dsu_floor() {
        let opps = [408, 816, 1200, 1800].map(|mhz| OperatingPoint {
            frequency_hz: mhz * 1_000_000,
            voltage_uv: None,
        });
        assert_eq!(fixed_target(816_000_000, &opps), 816_000_000);
        assert_eq!(fixed_target(1_800_000_000, &opps[..3]), 1_200_000_000);
        assert_eq!(fixed_target(408_000_000, &opps[2..]), 1_200_000_000);
    }

    /// A run retires before its vCPUs stop. Linux keeps reading `CUR_PERF`
    /// every scheduler tick until then, and a device error on that read stops
    /// the whole VM run as a fault, so retirement must keep serving reads while
    /// dropping the run from host aggregation.
    #[test]
    fn retiring_a_run_stops_raising_the_floor_without_erroring_cur_perf_reads() {
        let mut requests = GuestRequests::new();
        requests.bind(4, 7);
        requests
            .publish(4, 7, 0, 0, 1024)
            .expect("a bound run accepts hints");
        assert_eq!(requests.hints.len(), 1);

        requests.retire(4, 7);
        assert!(requests.serves(4, 7));
        assert!(requests.hints.is_empty());

        // A vCPU still executing guest code must not be able to re-raise the
        // host floor, and must not observe a device error either.
        requests
            .publish(4, 7, 0, 0, 1024)
            .expect("a retiring run keeps accepting hints");
        requests
            .publish(4, 7, 1, 0, 1024)
            .expect("a retiring run keeps accepting hints");
        assert!(requests.hints.is_empty());

        requests.unbind(4, 7);
        assert!(!requests.serves(4, 7));
        assert_eq!(
            requests.publish(4, 7, 0, 0, 256),
            Err(FrequencyError::NotReady)
        );
    }

    #[test]
    fn retiring_one_generation_keeps_other_generations_aggregating() {
        let mut requests = GuestRequests::new();
        requests.bind(4, 7);
        requests.bind(4, 8);
        requests
            .publish(4, 7, 0, 0, 1024)
            .expect("a bound run accepts hints");
        requests
            .publish(4, 8, 0, 0, 256)
            .expect("a bound run accepts hints");

        requests.retire(4, 7);
        assert!(requests.serves(4, 8));
        assert_eq!(requests.hints.len(), 1);
        assert_eq!(requests.hints[0].run_generation, 8);
    }
}
